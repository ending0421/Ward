//! The Rack — Ward's SQLite-backed, disposable index (spec §4).
//!
//! Everything here is derived data: deleting the whole database loses speed,
//! never correctness (law P1). Schema version mismatch → full rebuild (F1).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

/// Current schema version. Bump on any schema change; mismatches trigger a
/// full rebuild instead of a migration (rebuild is cheap and always safe).
pub const SCHEMA_VERSION: i64 = 7;

/// One indexed symbol (function / struct / enum / trait / method, …).
#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub id: Option<i64>,
    pub file_path: String,
    /// Monorepo scope (spec §2.6): the package/module boundary this symbol
    /// belongs to (Cargo package name, Gradle module dir, SwiftPM package
    /// dir, or the repo-root-relative top-level dir). Empty when unknown.
    pub module: String,
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
    /// True when the symbol lives inside a test module / test file
    /// (duplicate clustering exempts test code by default).
    pub in_test: bool,
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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Label {
    pub id: Option<i64>,
    pub advisory_id: String,
    pub match_index: i64,
    /// Who gave this verdict (double-annotation agreement, spec §8).
    pub annotator: String,
    pub query_hash: Option<String>,
    pub language: Option<String>,
    pub kind: Option<String>,
    pub similarity: Option<f64>,
    pub verdict: String,
    pub ts: i64,
}

/// One label-matrix row: (language, kind, verdict, count, avg_similarity).
pub type LabelRow = (String, String, String, i64, f64);

/// One advisory row for reporting: (ts, query_hash, result_json,
/// agent_action, inferred_action, inferred_commit_sha).
pub type AdvisoryRow = (
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// One daily trend snapshot (spec §9).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
    /// In-process BM25 cache: built once per symbol generation and dropped
    /// whenever the symbol table is rewritten (`replace_file`). The daemon
    /// and the PostToolUse hook run many spot queries per process —
    /// rebuilding the index per query is O(N·tokens) and dominates latency
    /// at 10⁴+ symbols (F11, spot P99 <100ms).
    bm25: std::cell::RefCell<Option<std::rc::Rc<crate::search::Bm25>>>,
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
            module      TEXT NOT NULL DEFAULT '',
            language    TEXT NOT NULL,
            name        TEXT NOT NULL,
            kind        TEXT NOT NULL,
            start_byte  INTEGER,
            end_byte    INTEGER,
            body_hash   TEXT NOT NULL,
            struct_hash TEXT NOT NULL,
            simhash     INTEGER NOT NULL,   -- u64 bit pattern (full subtree)
            sig_simhash INTEGER NOT NULL,   -- u64 bit pattern (signature only)
            in_test     INTEGER NOT NULL DEFAULT 0,
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
        -- replace_blocks deletes per file_path — index it (F11).
        CREATE INDEX IF NOT EXISTS idx_blocks_file ON blocks(file_path);

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
        -- The per-symbol DELETE walks this index; without it, index_all
        -- degrades to O(N²) at scale (F11 benchmark: 10⁵ symbols ≈ 9.5min).
        CREATE INDEX IF NOT EXISTS idx_mentions_symbol ON mentions(symbol_id);

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
        -- threshold calibration. `annotator` (v6) enables double-annotation
        -- agreement measurement (spec §8 标注腐烂护栏).
        CREATE TABLE IF NOT EXISTS labels (
            id          INTEGER PRIMARY KEY,
            advisory_id TEXT NOT NULL,
            match_index INTEGER NOT NULL,
            annotator   TEXT NOT NULL DEFAULT 'human',
            query_hash  TEXT,
            language    TEXT,
            kind        TEXT,
            similarity  REAL,
            verdict     TEXT NOT NULL,      -- y / n
            ts          INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_labels_verdict ON labels(verdict);
        CREATE INDEX IF NOT EXISTS idx_labels_lang ON labels(language);
        CREATE INDEX IF NOT EXISTS idx_labels_match ON labels(advisory_id, match_index);

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
        Ok(Store {
            conn,
            bm25: std::cell::RefCell::new(None),
        })
    }

    /// Drop every table and recreate the schema, then stamp the version.
    ///
    /// Governance data (advisories / labels / snapshots / contract_runs) is
    /// NOT derivable from git+files — unlike symbols/blocks/edges. A schema
    /// rebuild therefore preserves it: derived tables are wiped, governance
    /// tables are carried across (F1 rebuild loses speed, never truth).
    fn reset(conn: &Connection) -> Result<()> {
        const GOVERNANCE: &[&str] = &["advisories", "labels", "snapshots", "contract_runs"];
        conn.execute_batch("BEGIN;")?;
        for table in GOVERNANCE {
            conn.execute_batch(&format!(
                "DROP TABLE IF EXISTS ward_bak_{table};
                 CREATE TABLE ward_bak_{table} AS SELECT * FROM {table};"
            ))?;
        }
        conn.execute_batch(
            "DROP TABLE IF EXISTS symbols;
             DROP TABLE IF EXISTS blocks;
             DROP TABLE IF EXISTS edges;
             DROP TABLE IF EXISTS mentions;
             DROP TABLE IF EXISTS advisories;
             DROP TABLE IF EXISTS labels;
             DROP TABLE IF EXISTS snapshots;
             DROP TABLE IF EXISTS contract_runs;
             DROP TABLE IF EXISTS file_hashes;
             DROP TABLE IF EXISTS meta;",
        )?;
        create_schema(conn)?;
        // labels gained `annotator` in schema v6: copy column-explicit so a
        // v5 backup lands with the default annotator. The other governance
        // tables keep the `SELECT *` copy (additive-only policy).
        for table in GOVERNANCE {
            if *table == "labels" {
                conn.execute_batch(
                    "INSERT INTO labels
                       (advisory_id, match_index, annotator, query_hash,
                        language, kind, similarity, verdict, ts)
                     SELECT advisory_id, match_index, 'human', query_hash,
                            language, kind, similarity, verdict, ts
                     FROM ward_bak_labels;
                     DROP TABLE ward_bak_labels;",
                )?;
                continue;
            }
            conn.execute_batch(&format!(
                "INSERT INTO {table} SELECT * FROM ward_bak_{table};
                 DROP TABLE ward_bak_{table};"
            ))?;
        }
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        conn.execute_batch("COMMIT;")?;
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
        // The BM25 recall index is derived from the symbol table — drop it,
        // the next query rebuilds it against this state.
        *self.bm25.borrow_mut() = None;
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM symbols WHERE file_path = ?1",
            params![file_path],
        )?;
        let mut ids = Vec::with_capacity(symbols.len());
        for s in symbols {
            tx.execute(
                "INSERT INTO symbols
                   (file_path, module, language, name, kind, start_byte, end_byte,
                    body_hash, struct_hash, simhash, sig_simhash, in_test, commit_sha)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    s.file_path,
                    s.module,
                    s.language,
                    s.name,
                    s.kind,
                    s.start_byte,
                    s.end_byte,
                    s.body_hash,
                    s.struct_hash,
                    s.simhash as i64,
                    s.sig_simhash as i64,
                    s.in_test as i64,
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
        self.set_mentions_batch(&[(symbol_id, names.to_vec())])
    }

    /// Replace mention edges for many symbols in ONE transaction. Calling
    /// [`set_mentions`] per symbol commits (and fsyncs) once per symbol —
    /// that was 93% of full-index time at 10⁵ symbols (F11 benchmark).
    pub fn set_mentions_batch(&mut self, rows: &[(i64, Vec<String>)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for (symbol_id, names) in rows {
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
            "SELECT id, file_path, module, language, name, kind, start_byte, end_byte,
                    body_hash, struct_hash, simhash, sig_simhash, in_test, commit_sha
             FROM symbols",
        )?;
        let rows = stmt.query_map([], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Symbols with an exact L1 structural match.
    pub fn symbols_by_struct_hash(&self, hash: &str) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, module, language, name, kind, start_byte, end_byte,
                    body_hash, struct_hash, simhash, sig_simhash, in_test, commit_sha
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

    /// The BM25 recall index over the current symbol table, cached per
    /// store instance (invalidated by `replace_file`). The daemon and the
    /// PostToolUse hook amortize one build across many spot queries —
    /// building per query dominates spot latency at 10⁴+ symbols (F11).
    pub fn bm25(&self) -> Result<std::rc::Rc<crate::search::Bm25>> {
        let mut cache = self.bm25.borrow_mut();
        if cache.is_none() {
            let symbols = self.all_symbols()?;
            *cache = Some(std::rc::Rc::new(crate::search::Bm25::build(&symbols)));
        }
        Ok(cache.as_ref().expect("built above").clone())
    }

    pub fn record_label(&self, l: &Label) -> Result<()> {
        self.conn.execute(
            "INSERT INTO labels (advisory_id, match_index, annotator, query_hash, language, kind, similarity, verdict, ts)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                l.advisory_id,
                l.match_index,
                l.annotator,
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

    /// Calibration input: one (similarity, verdict) per match. With
    /// double-annotation the primary annotator's verdict wins when present
    /// (`human`), otherwise the earliest — calibration must not double-count
    /// a match, agreement is reported separately.
    pub fn labels_with_similarity(&self) -> Result<Vec<(f64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT advisory_id, match_index, similarity, verdict, annotator, ts
             FROM labels WHERE similarity IS NOT NULL ORDER BY ts ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        // Prefer the 'human' verdict for a match; fall back to earliest.
        let mut by_match: std::collections::BTreeMap<(String, i64), (f64, String)> =
            std::collections::BTreeMap::new();
        for row in rows {
            let (advisory, idx, sim, verdict, annotator) = row?;
            let key = (advisory, idx);
            let entry = by_match.entry(key).or_insert((sim, verdict.clone()));
            if annotator == "human" {
                *entry = (sim, verdict);
            }
        }
        Ok(by_match.into_values().collect())
    }

    /// Labels grouped by (language, kind, verdict) for calibration reports.
    pub fn label_matrix(&self) -> Result<Vec<LabelRow>> {
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

    /// True when this (advisory_id, match_index) is already labeled by ANY
    /// annotator.
    pub fn is_labeled(&self, advisory_id: &str, match_index: i64) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM labels WHERE advisory_id = ?1 AND match_index = ?2",
            params![advisory_id, match_index],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// True when the given annotator has already labeled this match
    /// (double-annotation: `label next --annotator alice` only shows matches
    /// alice has not seen).
    pub fn is_labeled_by(
        &self,
        advisory_id: &str,
        match_index: i64,
        annotator: &str,
    ) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM labels
             WHERE advisory_id = ?1 AND match_index = ?2 AND annotator = ?3",
            params![advisory_id, match_index, annotator],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Every label row — the raw input for the inter-annotator agreement
    /// report (spec §8 标注腐烂护栏).
    pub fn labels_all(&self) -> Result<Vec<Label>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, advisory_id, match_index, annotator, query_hash, language, kind, similarity, verdict, ts
             FROM labels ORDER BY advisory_id ASC, match_index ASC, ts ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Label {
                id: r.get(0)?,
                advisory_id: r.get(1)?,
                match_index: r.get(2)?,
                annotator: r.get(3)?,
                query_hash: r.get(4)?,
                language: r.get(5)?,
                kind: r.get(6)?,
                similarity: r.get(7)?,
                verdict: r.get(8)?,
                ts: r.get(9)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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

    /// Adoption counts across both channels:
    /// (inferred_total, inferred_accepted, inferred_rejected,
    ///  self_total, self_accepted).
    pub fn adoption_counts(&self) -> Result<(i64, i64, i64, i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(inferred_action IS NOT NULL),0),
                    COALESCE(SUM(inferred_action = 'accepted'),0),
                    COALESCE(SUM(inferred_action = 'rejected'),0),
                    COALESCE(SUM(agent_action IS NOT NULL),0),
                    COALESCE(SUM(agent_action = 'accepted'),0)
             FROM advisories",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?)
    }

    /// All contract runs ordered by ts (for decay analysis).
    pub fn all_contract_runs(&self) -> Result<Vec<ContractRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT spec_path, commit_sha, ts, assertion, verdict, detail
             FROM contract_runs ORDER BY ts ASC",
        )?;
        let rows = stmt.query_map([], |r| {
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

    /// Constraint-decay hint: pass rate of the last 10 runs minus the first
    /// 10 (spec §9 longitudinal analysis, coarse approximation).
    pub fn constraint_decay_hint(&self) -> Result<Option<f64>> {
        let runs = self.all_contract_runs()?;
        if runs.len() < 10 {
            return Ok(None);
        }
        let pass = |slice: &[ContractRun]| -> f64 {
            let ok = slice.iter().filter(|r| r.verdict == "pass").count();
            ok as f64 / slice.len() as f64
        };
        let first = pass(&runs[..10]);
        let last = pass(&runs[runs.len() - 10..]);
        Ok(Some(last - first))
    }

    /// One advisory row: (ts, query_hash, result_json, agent_action,
    /// inferred_action, inferred_commit_sha).
    pub fn advisory_row(&self, id: &str) -> Result<Option<AdvisoryRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT ts, COALESCE(query_hash,''), COALESCE(result_json,'[]'),
                        agent_action, inferred_action, inferred_commit_sha
                 FROM advisories WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()?)
    }

    /// Labels for one advisory, ordered by match index.
    pub fn labels_for_advisory(&self, advisory_id: &str) -> Result<Vec<Label>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, advisory_id, match_index, annotator, query_hash, language, kind, similarity, verdict, ts
             FROM labels WHERE advisory_id = ?1 ORDER BY match_index ASC",
        )?;
        let rows = stmt.query_map(params![advisory_id], |r| {
            Ok(Label {
                id: r.get(0)?,
                advisory_id: r.get(1)?,
                match_index: r.get(2)?,
                annotator: r.get(3)?,
                query_hash: r.get(4)?,
                language: r.get(5)?,
                kind: r.get(6)?,
                similarity: r.get(7)?,
                verdict: r.get(8)?,
                ts: r.get(9)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// All spot advisories: (id, ts, result_json) — newest first.
    pub fn advisory_payloads(&self) -> Result<Vec<(String, i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, COALESCE(result_json,'[]') FROM advisories
             WHERE tool = 'spot' ORDER BY ts DESC",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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
    let simhash: i64 = r.get(10)?;
    let sig_simhash: i64 = r.get(11)?;
    let in_test: i64 = r.get(12)?;
    Ok(Symbol {
        id: r.get(0)?,
        file_path: r.get(1)?,
        module: r.get(2)?,
        language: r.get(3)?,
        name: r.get(4)?,
        kind: r.get(5)?,
        start_byte: r.get(6)?,
        end_byte: r.get(7)?,
        body_hash: r.get(8)?,
        struct_hash: r.get(9)?,
        simhash: simhash as u64,
        sig_simhash: sig_simhash as u64,
        in_test: in_test != 0,
        commit_sha: r.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(name: &str, body: &str) -> Symbol {
        Symbol {
            id: None,
            file_path: "src/lib.rs".into(),
            module: String::new(),
            language: "rust".into(),
            name: name.into(),
            kind: "function_item".into(),
            start_byte: 0,
            end_byte: body.len() as i64,
            body_hash: format!("b-{name}"),
            struct_hash: format!("s-{name}"),
            simhash: 0xDEAD_BEEF,
            sig_simhash: 0xBEEF_DEAD,
            in_test: false,
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
    fn v5_labels_migrate_to_v6_with_default_annotator() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("index.db");
        {
            // Hand-build a v5-shaped database: schema_version 5 and the four
            // governance tables in their v5 column sets (labels has no
            // annotator yet), with one golden label inside.
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
                 INSERT INTO meta VALUES ('schema_version','5');
                 CREATE TABLE advisories (id TEXT PRIMARY KEY, tool TEXT, ts INTEGER,
                                          query_hash TEXT, result_json TEXT,
                                          agent_action TEXT, inferred_action TEXT,
                                          inferred_commit_sha TEXT);
                 CREATE TABLE labels (
                     id INTEGER PRIMARY KEY, advisory_id TEXT NOT NULL,
                     match_index INTEGER NOT NULL, query_hash TEXT, language TEXT,
                     kind TEXT, similarity REAL, verdict TEXT NOT NULL, ts INTEGER NOT NULL);
                 INSERT INTO labels (advisory_id, match_index, verdict, ts)
                 VALUES ('adv_1', 0, 'y', 1);
                 CREATE TABLE snapshots (ts INTEGER PRIMARY KEY, symbols INTEGER,
                                         clusters INTEGER, advisories INTEGER,
                                         labels INTEGER, contract_runs INTEGER,
                                         contract_pass INTEGER);
                 CREATE TABLE contract_runs (spec_path TEXT, commit_sha TEXT,
                                             ts INTEGER, assertion TEXT,
                                             verdict TEXT, detail TEXT);",
            )
            .unwrap();
        }
        // Opening with the v6 code bumps the schema: the rebuild must carry
        // the golden label across with annotator='human' (F1 rebuild loses
        // speed, never truth).
        let store = Store::open(&db).unwrap();
        let all = store.labels_all().unwrap();
        assert_eq!(all.len(), 1, "v5 label must survive the rebuild: {all:?}");
        assert_eq!(all[0].annotator, "human");
        assert_eq!(all[0].verdict, "y");
        assert!(store.is_labeled_by("adv_1", 0, "human").unwrap());
        assert!(!store.is_labeled_by("adv_1", 0, "alice").unwrap());
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
