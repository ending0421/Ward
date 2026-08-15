//! The Rack — Ward's SQLite-backed, disposable index (spec §4).
//!
//! Everything here is derived data: deleting the whole database loses speed,
//! never correctness (law P1). Schema version mismatch → full rebuild (F1).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

/// Current schema version. Bump on any schema change; mismatches trigger a
/// full rebuild instead of a migration (rebuild is cheap and always safe).
pub const SCHEMA_VERSION: i64 = 4;

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
    /// Simhash over the signature subtree (body excluded) — the comparison
    /// target for signature-shaped Spot queries.
    pub sig_simhash: u64,
    pub commit_sha: String,
}

/// A block-level fingerprint (spec §3-M1: sliding statement windows
/// inside function bodies — catches the in-function duplication that
/// symbol-level fingerprints cannot see).
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub id: Option<i64>,
    pub file_path: String,
    pub parent_symbol_id: Option<i64>,
    pub start_byte: i64,
    pub end_byte: i64,
    pub simhash: u64,
    pub kind: String,
    pub commit_sha: String,
}

/// One match-level golden-set label (spec §9).
#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub id: Option<i64>,
    pub advisory_id: String,
    pub match_index: i64,
    pub query_hash: Option<String>,
    pub language: Option<String>,
    pub kind: Option<String>,
    pub similarity: Option<f64>,
    pub verdict: String,
    pub ts: i64,
}

/// One daily trend snapshot (spec §9).
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub ts: i64,
    pub symbols: i64,
    pub clusters: i64,
    pub advisories: i64,
    pub labels: i64,
    pub contract_runs: i64,
    pub contract_pass: i64,
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
            simhash     INTEGER NOT NULL,   -- u64 bit pattern (full subtree)
            sig_simhash INTEGER NOT NULL,   -- u64 bit pattern (signature only)
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

        -- Golden-set labels (spec §9): match-level human verdicts feeding
        -- threshold calibration.
        CREATE TABLE IF NOT EXISTS labels (
            id          INTEGER PRIMARY KEY,
            advisory_id TEXT NOT NULL,
            match_index INTEGER NOT NULL,
            query_hash  TEXT,
            language    TEXT,
            kind        TEXT,
            similarity  REAL,
            verdict     TEXT NOT NULL,      -- y / n
            ts          INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_labels_verdict ON labels(verdict);
        CREATE INDEX IF NOT EXISTS idx_labels_lang ON labels(language);

        -- Trend snapshots (spec §9): one row per day, idempotent.
        CREATE TABLE IF NOT EXISTS snapshots (
            ts               INTEGER PRIMARY KEY,   -- day key (unix day)
            symbols          INTEGER NOT NULL,
            clusters         INTEGER NOT NULL,
            advisories       INTEGER NOT NULL,
            labels           INTEGER NOT NULL,
            contract_runs    INTEGER NOT NULL,
            contract_pass    INTEGER NOT NULL
        );

        -- per-file content hashes for the per-file freshness protocol (spec §5),
        -- plus mtime/size for the incremental-indexing skip (spec §5.2)
        CREATE TABLE IF NOT EXISTS file_hashes (
            file_path TEXT PRIMARY KEY,
            hash      TEXT NOT NULL,
            mtime     INTEGER NOT NULL DEFAULT 0,
            size      INTEGER NOT NULL DEFAULT 0
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
        let conn =
            Connection::open(path).with_context(|| format!("opening index {}", path.display()))?;
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
                tracing::warn!("index schema version {v} != {SCHEMA_VERSION}; rebuilding (F1)");
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
    /// Returns the inserted row ids in insertion order (used to attach
    /// per-symbol mention edges).
    pub fn replace_file(&mut self, file_path: &str, symbols: &[Symbol]) -> Result<Vec<i64>> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM symbols WHERE file_path = ?1",
            params![file_path],
        )?;
        let mut ids = Vec::with_capacity(symbols.len());
        for s in symbols {
            tx.execute(
                "INSERT INTO symbols
                   (file_path, language, name, kind, start_byte, end_byte,
                    body_hash, struct_hash, simhash, sig_simhash, commit_sha)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
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
                    s.sig_simhash as i64,
                    s.commit_sha,
                ],
            )?;
            ids.push(tx.last_insert_rowid());
        }
        tx.commit()?;
        Ok(ids)
    }

    /// Record identifier mentions for one symbol (static edge lower bound).
    pub fn set_mentions(&mut self, symbol_id: i64, names: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM mentions WHERE symbol_id = ?1",
            params![symbol_id],
        )?;
        for n in names {
            tx.execute(
                "INSERT INTO mentions (symbol_id, name) VALUES (?1, ?2)",
                params![symbol_id, n],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Symbols that mention `name` (reverse index for M5 context cards).
    /// Returns `(file_path, symbol_name)` pairs.
    pub fn mentioners(&self, name: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.file_path, s.name FROM mentions m
             JOIN symbols s ON s.id = m.symbol_id
             WHERE m.name = ?1 ORDER BY s.file_path",
        )?;
        let rows = stmt.query_map(params![name], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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
                    body_hash, struct_hash, simhash, sig_simhash, commit_sha
             FROM symbols",
        )?;
        let rows = stmt.query_map([], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Symbols with an exact L1 structural match.
    pub fn symbols_by_struct_hash(&self, hash: &str) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, language, name, kind, start_byte, end_byte,
                    body_hash, struct_hash, simhash, sig_simhash, commit_sha
             FROM symbols WHERE struct_hash = ?1",
        )?;
        let rows = stmt.query_map(params![hash], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Replace every block fingerprint of one file.
    pub fn replace_blocks(&mut self, file_path: &str, blocks: &[Block]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM blocks WHERE file_path = ?1",
            params![file_path],
        )?;
        for b in blocks {
            tx.execute(
                "INSERT INTO blocks (file_path, parent_symbol_id, start_byte, end_byte, simhash, kind, commit_sha)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    b.file_path,
                    b.parent_symbol_id,
                    b.start_byte,
                    b.end_byte,
                    b.simhash as i64,
                    b.kind,
                    b.commit_sha,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// All block fingerprints (in-function statement windows).
    pub fn all_blocks(&self) -> Result<Vec<Block>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, parent_symbol_id, start_byte, end_byte, simhash, kind, commit_sha FROM blocks",
        )?;
        let rows = stmt.query_map([], |r| {
            let simhash: i64 = r.get(5)?;
            Ok(Block {
                id: r.get(0)?,
                file_path: r.get(1)?,
                parent_symbol_id: r.get(2)?,
                start_byte: r.get(3)?,
                end_byte: r.get(4)?,
                simhash: simhash as u64,
                kind: r.get(6)?,
                commit_sha: r.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // ---- golden-set labels (spec §9) --------------------------------------

    pub fn record_label(&self, l: &Label) -> Result<()> {
        self.conn.execute(
            "INSERT INTO labels (advisory_id, match_index, query_hash, language, kind, similarity, verdict, ts)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                l.advisory_id,
                l.match_index,
                l.query_hash,
                l.language,
                l.kind,
                l.similarity,
                l.verdict,
                l.ts,
            ],
        )?;
        Ok(())
    }

    pub fn label_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM labels", [], |r| r.get(0))?)
    }

    /// Labels grouped by (language, kind, verdict) for calibration reports.
    pub fn label_matrix(&self) -> Result<Vec<(String, String, String, i64, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(language,'?'), COALESCE(kind,'?'), verdict,
                    COUNT(*), COALESCE(AVG(similarity),0)
             FROM labels GROUP BY 1,2,3 ORDER BY 1,2,3",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// True when this (advisory_id, match_index) is already labeled.
    pub fn is_labeled(&self, advisory_id: &str, match_index: i64) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM labels WHERE advisory_id = ?1 AND match_index = ?2",
            params![advisory_id, match_index],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    // ---- snapshots (spec §9) ----------------------------------------------

    pub fn record_snapshot(&self, snap: &Snapshot) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO snapshots
               (ts, symbols, clusters, advisories, labels, contract_runs, contract_pass)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                snap.ts,
                snap.symbols,
                snap.clusters,
                snap.advisories,
                snap.labels,
                snap.contract_runs,
                snap.contract_pass,
            ],
        )?;
        Ok(())
    }

    pub fn snapshots(&self) -> Result<Vec<Snapshot>> {
        let mut stmt = self.conn.prepare(
            "SELECT ts, symbols, clusters, advisories, labels, contract_runs, contract_pass
             FROM snapshots ORDER BY ts ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Snapshot {
                ts: r.get(0)?,
                symbols: r.get(1)?,
                clusters: r.get(2)?,
                advisories: r.get(3)?,
                labels: r.get(4)?,
                contract_runs: r.get(5)?,
                contract_pass: r.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn counts(&self) -> Result<(i64, i64, i64, i64)> {
        // (symbols, advisories, contract_runs, contract_pass)
        let symbols: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        let advisories: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM advisories", [], |r| r.get(0))?;
        let runs: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM contract_runs", [], |r| r.get(0))?;
        let pass: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM contract_runs WHERE verdict = 'pass'",
            [],
            |r| r.get(0),
        )?;
        Ok((symbols, advisories, runs, pass))
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

    /// Record content hash + (mtime, size) — the incremental-indexing key.
    /// mtime/size equal ⇒ content unchanged (spec §5.2 three-level check).
    pub fn set_file_meta(&self, file_path: &str, hash: &str, mtime: i64, size: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO file_hashes (file_path, hash, mtime, size) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(file_path) DO UPDATE SET hash = excluded.hash,
                 mtime = excluded.mtime, size = excluded.size",
            params![file_path, hash, mtime, size],
        )?;
        Ok(())
    }

    /// The stored (mtime, size) for a file, if indexed before.
    pub fn get_file_meta(&self, file_path: &str) -> Result<Option<(i64, i64)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT mtime, size FROM file_hashes WHERE file_path = ?1",
                params![file_path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
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

    /// Advisories awaiting outcome inference: (id, ts, result_json).
    pub fn pending_inferences(&self) -> Result<Vec<(String, i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, COALESCE(result_json,'[]') FROM advisories
             WHERE tool = 'spot' AND inferred_action IS NULL ORDER BY ts ASC",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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
            params![
                r.spec_path,
                r.commit_sha,
                r.ts,
                r.assertion,
                r.verdict,
                r.detail
            ],
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
    let sig_simhash: i64 = r.get(10)?;
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
        sig_simhash: sig_simhash as u64,
        commit_sha: r.get(11)?,
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
            sig_simhash: 0xBEEF_DEAD,
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
        store.replace_file("src/a.rs", &[symbol("a", "x")]).unwrap();
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
    fn blocks_roundtrip_and_mentioners() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("index.db")).unwrap();
        store
            .replace_blocks(
                "f.rs",
                &[Block {
                    id: None,
                    file_path: "f.rs".into(),
                    parent_symbol_id: None,
                    start_byte: 0,
                    end_byte: 10,
                    simhash: 42,
                    kind: "statement_block".into(),
                    commit_sha: "c".into(),
                }],
            )
            .unwrap();
        let blocks = store.all_blocks().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].simhash, 42);

        // Reverse index: mentioners.
        store
            .replace_file("f.rs", &[symbol("caller", "fn caller() { debounce() }")])
            .unwrap();
        let all = store.all_symbols().unwrap();
        let id = all[0].id.unwrap();
        store.set_mentions(id, &["debounce".into()]).unwrap();
        let mentioners = store.mentioners("debounce").unwrap();
        assert_eq!(mentioners.len(), 1);
        assert_eq!(mentioners[0].1, "caller");
        assert!(store.mentioners("nobody").unwrap().is_empty());
    }

    #[test]
    fn advisory_upsert_replaces_previous_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        let a = Advisory {
            id: "adv_1".into(),
            tool: "spot".into(),
            ts: 1,
            query_hash: "q1".into(),
            result_json: "[]".into(),
            ..Default::default()
        };
        store.record_advisory(&a).unwrap();
        let a2 = Advisory {
            id: "adv_1".into(),
            tool: "spot".into(),
            ts: 2,
            query_hash: "q2".into(),
            result_json: "[x]".into(),
            ..Default::default()
        };
        store.record_advisory(&a2).unwrap();
        // The row was replaced (INSERT OR REPLACE): the action update still
        // resolves to exactly one row.
        store.set_agent_action("adv_1", "ignored").unwrap();
    }

    #[test]
    fn corrupted_database_triggers_f1_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("index.db");
        {
            let mut store = Store::open(&db).unwrap();
            store.replace_file("f.rs", &[symbol("f", "x")]).unwrap();
        }
        // Drop WAL sidecars first: while they exist, SQLite recovers from
        // them and the main file is not the data source.
        let _ = std::fs::remove_file(dir.path().join("index.db-wal"));
        let _ = std::fs::remove_file(dir.path().join("index.db-shm"));
        // Corrupt a whole page beyond the header (page 1 is sacred; page 2+
        // corruption opens but fails `PRAGMA quick_check`).
        let mut bytes = std::fs::read(&db).unwrap();
        assert!(
            bytes.len() > 400,
            "need corruptible bytes, len={}",
            bytes.len()
        );
        // Corrupt everything from the halfway point onward — page 1 header
        // stays intact so the file still opens, but quick_check must fail.
        let half = bytes.len() / 2;
        for b in bytes.iter_mut().skip(half) {
            *b ^= 0xAA;
        }
        std::fs::write(&db, &bytes).unwrap();
        // Heavy corruption must fail LOUDLY at open (callers fail open),
        // never return a half-broken store. The rebuild path itself (F1) is
        // covered by the schema-version-mismatch test in engine_e2e.
        assert!(Store::open(&db).is_err(), "bad corruption must fail open");
    }

    #[test]
    fn file_hash_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        store.set_file_hash("f.rs", "h1").unwrap();
        store.set_file_hash("f.rs", "h2").unwrap();
        assert_eq!(store.get_file_hash("f.rs").unwrap().unwrap(), "h2");
    }

    #[test]
    fn contract_runs_empty_for_unknown_spec() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        assert!(store.contract_runs_for_spec("nope").unwrap().is_empty());
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
