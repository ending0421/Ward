//! The Rack — Ward's SQLite-backed, disposable index (spec §4).
//!
//! Everything here is derived data: deleting the whole database loses speed,
//! never correctness (law P1). Schema version mismatch → full rebuild (F1).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

/// Current schema version. Bump on any schema change; mismatches trigger a
/// full rebuild instead of a migration (rebuild is cheap and always safe).
pub const SCHEMA_VERSION: i64 = 1;

/// One indexed symbol (function / struct / enum / trait / method, …).
#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub id: Option<i64>,
    pub file_path: String,
    pub language: String,
    pub name: String,
    pub kind: String,
    pub start_byte: i64,
    pub end_byte: i64,
    pub body_hash: String,
    pub struct_hash: String,
    pub simhash: u64,
    pub commit_sha: String,
}

/// An advisory outcome record (M1 feedback loop, spec §4).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Advisory {
    pub id: String,
    pub tool: String,
    pub ts: i64,
    pub query_hash: String,
    pub result_json: String,
    pub agent_action: Option<String>,
    pub inferred_action: Option<String>,
    pub inferred_commit_sha: Option<String>,
}

/// A spec assertion run record (M4, spec §4).
#[derive(Debug, Clone, PartialEq)]
pub struct ContractRun {
    pub spec_path: String,
    pub commit_sha: String,
    pub ts: i64,
    pub assertion: String,
    pub verdict: String,
    pub detail: String,
}

/// Open handle to the Ward index database.
pub struct Store {
    conn: Connection,
}

fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        -- spec §4: symbol table
        CREATE TABLE IF NOT EXISTS symbols (
            id          INTEGER PRIMARY KEY,
            file_path   TEXT NOT NULL,
            language    TEXT NOT NULL,
            name        TEXT NOT NULL,
            kind        TEXT NOT NULL,
            start_byte  INTEGER,
            end_byte    INTEGER,
            body_hash   TEXT NOT NULL,
            struct_hash TEXT NOT NULL,
            simhash     INTEGER NOT NULL,   -- u64 bit pattern
            commit_sha  TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_symbols_struct ON symbols(struct_hash);
        CREATE INDEX IF NOT EXISTS idx_symbols_lang ON symbols(language);
        CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_path);
        CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);

        -- spec §4: block fingerprints (populated from Phase 1)
        CREATE TABLE IF NOT EXISTS blocks (
            id               INTEGER PRIMARY KEY,
            file_path        TEXT NOT NULL,
            parent_symbol_id INTEGER,
            start_byte       INTEGER,
            end_byte         INTEGER,
            simhash          INTEGER NOT NULL,
            kind             TEXT,
            commit_sha       TEXT NOT NULL
        );

        -- spec §4: dependency edges (static, lower-bound estimate)
        CREATE TABLE IF NOT EXISTS edges (
            src_id INTEGER NOT NULL,
            dst_id INTEGER NOT NULL,
            kind   TEXT NOT NULL,
            PRIMARY KEY (src_id, dst_id, kind)
        );

        -- implementation detail: identifier mentions per symbol.
        -- The edge builder for the static call graph; a *lower bound* on
        -- references (dynamic dispatch is invisible to it).
        CREATE TABLE IF NOT EXISTS mentions (
            symbol_id INTEGER NOT NULL,
            name      TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_mentions_name ON mentions(name);

        -- spec §4: advisory feedback loop
        CREATE TABLE IF NOT EXISTS advisories (
            id                  TEXT PRIMARY KEY,
            tool                TEXT NOT NULL,
            ts                  INTEGER NOT NULL,
            query_hash          TEXT,
            result_json         TEXT,
            agent_action        TEXT,   -- self-reported: accepted/ignored/dismissed/unknown
            inferred_action     TEXT,   -- outcome-inferred: accepted/reused-ish/rejected/unknown
            inferred_commit_sha TEXT
        );

        -- spec §4: spec assertion runs
        CREATE TABLE IF NOT EXISTS contract_runs (
            spec_path  TEXT,
            commit_sha TEXT,
            ts         INTEGER,
            assertion  TEXT,
            verdict    TEXT,   -- pass/fail/unknown/deferred
            detail     TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_contract_runs_spec ON contract_runs(spec_path);

        -- per-file content hashes for the per-file freshness protocol (spec §5)
        CREATE TABLE IF NOT EXISTS file_hashes (
            file_path TEXT PRIMARY KEY,
            hash      TEXT NOT NULL
        );
        "#,
    )
    .context("creating schema")
}

impl Store {
    /// Open (or create) the index at `path`, enforcing the schema version.
    ///
    /// A version mismatch wipes and rebuilds the database — the documented
    /// F1 behavior ("delete and rebuild, never migrate in place").
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening index {}", path.display()))?;
        create_schema(&conn)?;
        let version: Option<i64> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()?
            .and_then(|v: String| v.parse().ok());
        match version {
            None => {
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                    params![SCHEMA_VERSION.to_string()],
                )?;
            }
            Some(v) if v != SCHEMA_VERSION => {
                tracing::warn!(
                    "index schema version {v} != {SCHEMA_VERSION}; rebuilding (F1)"
                );
                Self::reset(&conn)?;
            }
            Some(_) => {}
        }
        // Lightweight integrity check on open (F1 detection).
        let integrity: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            tracing::warn!("index integrity check failed ({integrity}); rebuilding (F1)");
            Self::reset(&conn)?;
        }
        Ok(Store { conn })
    }

    /// Drop every table and recreate the schema, then stamp the version.
    fn reset(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "DROP TABLE IF EXISTS symbols;
             DROP TABLE IF EXISTS blocks;
             DROP TABLE IF EXISTS edges;
             DROP TABLE IF EXISTS mentions;
             DROP TABLE IF EXISTS advisories;
             DROP TABLE IF EXISTS contract_runs;
             DROP TABLE IF EXISTS file_hashes;
             DROP TABLE IF EXISTS meta;",
        )?;
        create_schema(conn)?;
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// Default index path for a repository: `<repo>/.ward/index.db`.
    pub fn default_path(repo_root: &Path) -> PathBuf {
        repo_root.join(".ward").join("index.db")
    }

    // ---- indexing --------------------------------------------------------

    /// Replace every symbol of one file with the newly parsed set.
    pub fn replace_file(&mut self, file_path: &str, symbols: &[Symbol]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM symbols WHERE file_path = ?1", params![file_path])?;
        for s in symbols {
            tx.execute(
                "INSERT INTO symbols
                   (file_path, language, name, kind, start_byte, end_byte,
                    body_hash, struct_hash, simhash, commit_sha)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    s.file_path,
                    s.language,
                    s.name,
                    s.kind,
                    s.start_byte,
                    s.end_byte,
                    s.body_hash,
                    s.struct_hash,
                    s.simhash as i64,
                    s.commit_sha,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Record identifier mentions for one symbol (static edge lower bound).
    pub fn set_mentions(&mut self, symbol_id: i64, names: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM mentions WHERE symbol_id = ?1", params![symbol_id])?;
        for n in names {
            tx.execute(
                "INSERT INTO mentions (symbol_id, name) VALUES (?1, ?2)",
                params![symbol_id, n],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Count of symbols that mention `name` — the "at least N callers"
    /// lower-bound estimate used by M2 impact analysis.
    pub fn mention_count(&self, name: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(DISTINCT symbol_id) FROM mentions WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )?)
    }

    /// All symbols (used by the BM25 layer; a single repository fits in
    /// memory by design — 10⁴–10⁵ symbols, spec §4).
    pub fn all_symbols(&self) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, language, name, kind, start_byte, end_byte,
                    body_hash, struct_hash, simhash, commit_sha
             FROM symbols",
        )?;
        let rows = stmt.query_map([], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Symbols with an exact L1 structural match.
    pub fn symbols_by_struct_hash(&self, hash: &str) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, language, name, kind, start_byte, end_byte,
                    body_hash, struct_hash, simhash, commit_sha
             FROM symbols WHERE struct_hash = ?1",
        )?;
        let rows = stmt.query_map(params![hash], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // ---- freshness (spec §5) ---------------------------------------------

    pub fn set_file_hash(&self, file_path: &str, hash: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO file_hashes (file_path, hash) VALUES (?1, ?2)
             ON CONFLICT(file_path) DO UPDATE SET hash = excluded.hash",
            params![file_path, hash],
        )?;
        Ok(())
    }

    pub fn get_file_hash(&self, file_path: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT hash FROM file_hashes WHERE file_path = ?1",
                params![file_path],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_last_indexed_sha(&self, sha: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES ('last_indexed_sha', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![sha],
        )?;
        Ok(())
    }

    pub fn last_indexed_sha(&self) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'last_indexed_sha'",
                [],
                |r| r.get(0),
            )
            .optional()?)
    }

    // ---- advisories (M1 feedback loop, spec §4) ---------------------------

    pub fn record_advisory(&self, a: &Advisory) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO advisories
               (id, tool, ts, query_hash, result_json, agent_action,
                inferred_action, inferred_commit_sha)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                a.id,
                a.tool,
                a.ts,
                a.query_hash,
                a.result_json,
                a.agent_action,
                a.inferred_action,
                a.inferred_commit_sha,
            ],
        )?;
        Ok(())
    }

    /// Update the self-reported agent action for an advisory.
    pub fn set_agent_action(&self, id: &str, action: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE advisories SET agent_action = ?1 WHERE id = ?2",
            params![action, id],
        )?;
        anyhow::ensure!(n > 0, "unknown advisory id {id}");
        Ok(())
    }

    /// Update the outcome-inferred action for an advisory (spec §3-M1:
    /// `accepted / reused-ish / rejected / unknown`).
    pub fn set_inferred_action(&self, id: &str, action: &str, commit_sha: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE advisories SET inferred_action = ?1, inferred_commit_sha = ?2 WHERE id = ?3",
            params![action, commit_sha, id],
        )?;
        anyhow::ensure!(n > 0, "unknown advisory id {id}");
        Ok(())
    }

    // ---- contract runs (M4, spec §4) --------------------------------------

    pub fn record_contract_run(&self, r: &ContractRun) -> Result<()> {
        self.conn.execute(
            "INSERT INTO contract_runs (spec_path, commit_sha, ts, assertion, verdict, detail)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![r.spec_path, r.commit_sha, r.ts, r.assertion, r.verdict, r.detail],
        )?;
        Ok(())
    }

    /// Verdict history for a spec, oldest first (M4 constraint-decay
    /// longitudinal analysis, spec §9).
    pub fn contract_runs_for_spec(&self, spec_path: &str) -> Result<Vec<ContractRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT spec_path, commit_sha, ts, assertion, verdict, detail
             FROM contract_runs WHERE spec_path = ?1 ORDER BY ts ASC",
        )?;
        let rows = stmt.query_map(params![spec_path], |r| {
            Ok(ContractRun {
                spec_path: r.get(0)?,
                commit_sha: r.get(1)?,
                ts: r.get(2)?,
                assertion: r.get(3)?,
                verdict: r.get(4)?,
                detail: r.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

fn row_to_symbol(r: &rusqlite::Row<'_>) -> rusqlite::Result<Symbol> {
    let simhash: i64 = r.get(9)?;
    Ok(Symbol {
        id: r.get(0)?,
        file_path: r.get(1)?,
        language: r.get(2)?,
        name: r.get(3)?,
        kind: r.get(4)?,
        start_byte: r.get(5)?,
        end_byte: r.get(6)?,
        body_hash: r.get(7)?,
        struct_hash: r.get(8)?,
        simhash: simhash as u64,
        commit_sha: r.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(name: &str, body: &str) -> Symbol {
        Symbol {
            id: None,
            file_path: "src/lib.rs".into(),
            language: "rust".into(),
            name: name.into(),
            kind: "function_item".into(),
            start_byte: 0,
            end_byte: body.len() as i64,
            body_hash: format!("b-{name}"),
            struct_hash: format!("s-{name}"),
            simhash: 0xDEAD_BEEF,
            commit_sha: "abc123".into(),
        }
    }

    #[test]
    fn replace_file_and_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("index.db")).unwrap();
        store
            .replace_file("src/lib.rs", &[symbol("debounce", "fn debounce() {}")])
            .unwrap();
        store
            .replace_file("src/lib.rs", &[symbol("debounce", "fn debounce() {}")])
            .unwrap();
        let all = store.all_symbols().unwrap();
        assert_eq!(all.len(), 1, "replace-by-file must not duplicate rows");
    }

    #[test]
    fn struct_hash_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("index.db")).unwrap();
        store
            .replace_file("src/a.rs", &[symbol("a", "x")])
            .unwrap();
        let hits = store.symbols_by_struct_hash("s-a").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "a");
    }

    #[test]
    fn mention_count_is_lower_bound() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("index.db")).unwrap();
        store.replace_file("src/a.rs", &[symbol("a", "x")]).unwrap();
        let all = store.all_symbols().unwrap();
        let id = all[0].id.unwrap();
        store
            .set_mentions(id, &["debounce".into(), "throttle".into()])
            .unwrap();
        assert_eq!(store.mention_count("debounce").unwrap(), 1);
        assert_eq!(store.mention_count("nope").unwrap(), 0);
    }

    #[test]
    fn advisory_roundtrip_and_actions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        let a = Advisory {
            id: "adv_1".into(),
            tool: "spot".into(),
            ts: 1,
            query_hash: "q".into(),
            result_json: "{}".into(),
            ..Default::default()
        };
        store.record_advisory(&a).unwrap();
        store.set_agent_action("adv_1", "accepted").unwrap();
        store
            .set_inferred_action("adv_1", "rejected", "deadbeef")
            .unwrap();
        assert!(store.set_agent_action("adv_404", "x").is_err());
    }

    #[test]
    fn contract_runs_are_ordered_by_ts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        for (ts, verdict) in [(2, "fail"), (1, "pass")] {
            store
                .record_contract_run(&ContractRun {
                    spec_path: "specs/task.md".into(),
                    commit_sha: "c".into(),
                    ts,
                    assertion: "must_pass".into(),
                    verdict: verdict.into(),
                    detail: String::new(),
                })
                .unwrap();
        }
        let runs = store.contract_runs_for_spec("specs/task.md").unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].verdict, "pass", "must be ordered oldest first");
    }

    #[test]
    fn freshness_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        store.set_file_hash("src/lib.rs", "h1").unwrap();
        assert_eq!(store.get_file_hash("src/lib.rs").unwrap().unwrap(), "h1");
        assert!(store.get_file_hash("src/other.rs").unwrap().is_none());
        store.set_last_indexed_sha("abc").unwrap();
        assert_eq!(store.last_indexed_sha().unwrap().unwrap(), "abc");
    }
}
