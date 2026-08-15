//! Ward scale benchmark (spec F11): a deterministic synthetic-repo generator
//! plus timed runs of the real engine (index / spot / clusters).
//!
//! Synthetic repos keep the benchmark reproducible and free of proprietary
//! code; real-repo validation is a separate, long-cycle activity (spec §9).
//! Two sizes are the standing baseline: 10⁴ and 10⁵ symbols — the spec's
//! hard promises are "index <10min and spot P99 <100ms at ≤5×10⁵ symbols".

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde::Serialize;
use ward_core::config::WardConfig;
use ward_core::store::Store;

#[derive(Parser)]
#[command(name = "ward-bench", about = "Ward scale benchmark (spec F11)")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate a deterministic synthetic repository.
    Gen {
        /// Output directory (created; existing files are overwritten).
        #[arg(long, default_value = "/tmp/ward-bench-repo")]
        out: PathBuf,
        /// Total symbol count across all languages.
        #[arg(long, default_value_t = 10_000)]
        symbols: usize,
        /// Comma-separated languages (rust,kotlin,swift).
        #[arg(long, default_value = "rust,kotlin,swift")]
        languages: String,
        /// Fraction of symbols that are near-duplicates of earlier ones
        /// (exercises L1/L2 and clustering).
        #[arg(long, default_value_t = 0.3)]
        cluster_ratio: f64,
        /// PRNG seed — same seed, same repo.
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// Functions per source file.
        #[arg(long, default_value_t = 200)]
        symbols_per_file: usize,
    },
    /// Time the real engine over an existing repository.
    Run {
        /// Repository to measure (e.g. the output of `gen`).
        #[arg(long, default_value = "/tmp/ward-bench-repo")]
        repo: PathBuf,
        /// Number of spot queries for latency percentiles.
        #[arg(long, default_value_t = 100)]
        queries: usize,
    },
}

// ---------------------------------------------------------------- generator

/// One template body per language: `{name}` and `{lit}` are substituted.
const TEMPLATES: &[(&str, &str)] = &[
    (
        "rust",
        "pub fn {name}(a: u64, b: u64) -> u64 {\n    let mut x = a.wrapping_add({lit});\n    x = x.wrapping_mul(b.wrapping_add({lit}));\n    x\n}\n",
    ),
    (
        "kotlin",
        "fun {name}(a: Long, b: Long): Long {\n    var x = a + {lit}L\n    x = x * (b + {lit}L)\n    return x\n}\n",
    ),
    (
        "swift",
        "func {name}(_ a: UInt64, _ b: UInt64) -> UInt64 {\n    var x = a &+ {lit}\n    x = x &* (b &+ {lit})\n    return x\n}\n",
    ),
];

/// Tiny deterministic PRNG (xorshift64*) — std has no seeded RNG.
struct Prng(u64);
impl Prng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Generate the synthetic repo. Returns the number of files written.
pub fn gen_repo(
    out: &Path,
    symbols: usize,
    languages: &[&str],
    cluster_ratio: f64,
    seed: u64,
    per_file: usize,
) -> Result<usize> {
    let langs: Vec<(&str, &str)> = languages
        .iter()
        .filter_map(|l| {
            let l = l.trim();
            TEMPLATES
                .iter()
                .find(|(name, _)| *name == l)
                .map(|(name, tpl)| (*name, *tpl))
        })
        .collect();
    anyhow::ensure!(!langs.is_empty(), "no known language in {languages:?}");
    let mut rng = Prng(seed);
    let mut files = 0;
    let per_lang = symbols.div_ceil(langs.len());
    let mut total = 0usize;
    for (lang, tpl) in &langs {
        // Duplicates must reuse the SAME language's template — a rust
        // `pub fn` inside a .kt file would be a parse error (F3 skip).
        let mut pool: Vec<&str> = Vec::new();
        let ext = match *lang {
            "rust" => "rs",
            "kotlin" => "kt",
            _ => "swift",
        };
        let mut remaining = per_lang.min(symbols - total);
        let mut module = 0;
        while remaining > 0 {
            let in_file = remaining.min(per_file);
            let dir = out.join("src").join(lang);
            std::fs::create_dir_all(&dir)?;
            let mut buf = String::new();
            for i in 0..in_file {
                let idx = total + i;
                let name = format!("f{idx:06}");
                let is_dup =
                    i > 0 && !pool.is_empty() && (rng.below(1000) as f64) / 1000.0 < cluster_ratio;
                let src = if is_dup {
                    let psrc = pool[rng.below(pool.len())];
                    let lit = rng.below(10_000);
                    // copy-then-modify: same structure, different literal
                    psrc.replace("{name}", &name)
                        .replace("{lit}", &lit.to_string())
                } else {
                    let lit = rng.below(10_000);
                    tpl.replace("{name}", &name)
                        .replace("{lit}", &lit.to_string())
                };
                pool.push(tpl);
                buf.push_str(&src);
                if *lang == "kotlin" || *lang == "swift" {
                    buf.push('\n');
                }
            }
            let path = dir.join(format!("mod{module:04}.{ext}"));
            std::fs::write(&path, buf)?;
            files += 1;
            total += in_file;
            remaining -= in_file;
            module += 1;
        }
    }
    Ok(files)
}

// ---------------------------------------------------------------- runner

#[derive(Debug, Serialize)]
pub struct BenchReport {
    pub symbols: usize,
    pub files: usize,
    pub full_index_secs: f64,
    pub incremental_index_secs: f64,
    pub spot_queries: usize,
    pub spot_p50_ms: f64,
    pub spot_p99_ms: f64,
    pub spot_max_ms: f64,
    pub clusters: usize,
    pub cluster_secs: f64,
    pub db_bytes: u64,
}

/// Run the real engine over `repo` and measure the standing F11 baselines.
pub fn run_bench(repo: &Path, queries: usize) -> Result<BenchReport> {
    let cfg = WardConfig::default();

    let t = Instant::now();
    let report = ward_core::index::index_repo(repo, &cfg)?;
    let full_index_secs = t.elapsed().as_secs_f64();

    let t = Instant::now();
    ward_core::index::index_repo(repo, &cfg)?;
    let incremental_index_secs = t.elapsed().as_secs_f64();

    let store = Store::open(&Store::default_path(repo))?;
    let symbols = store.all_symbols()?;
    let n = symbols.len();

    // Evenly spaced deterministic query sample: signature = the symbol's
    // own source (worst realistic case: a real hit).
    let mut lat_ms = Vec::with_capacity(queries);
    for q in 0..queries {
        let sym = &symbols[(q * n) / queries];
        let lang = ward_core::lang::Language::from_name(&sym.language);
        let source = std::fs::read_to_string(repo.join(&sym.file_path)).unwrap_or_default();
        let sig = source
            .get(sym.start_byte as usize..sym.end_byte as usize)
            .unwrap_or("")
            .to_string();
        let t = Instant::now();
        let _ = ward_core::search::spot(
            repo,
            &store,
            &cfg,
            "benchmark probe",
            Some(&sig),
            None,
            lang,
        )?;
        lat_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    lat_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| -> f64 {
        let i = ((queries as f64) * p).ceil() as usize;
        lat_ms[i.saturating_sub(1)]
    };

    let t = Instant::now();
    let clusters = ward_core::cluster::cluster_duplicates(&store, cfg.thresholds.strong)?;
    let cluster_secs = t.elapsed().as_secs_f64();

    let db_bytes = std::fs::metadata(Store::default_path(repo))
        .map(|m| m.len())
        .unwrap_or(0);

    Ok(BenchReport {
        symbols: n,
        files: report.files_indexed,
        full_index_secs,
        incremental_index_secs,
        spot_queries: queries,
        spot_p50_ms: pct(0.50),
        spot_p99_ms: pct(0.99),
        spot_max_ms: lat_ms.last().copied().unwrap_or(0.0),
        clusters: clusters.clusters.len(),
        cluster_secs,
        db_bytes,
    })
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.cmd {
        Cmd::Gen {
            out,
            symbols,
            languages,
            cluster_ratio,
            seed,
            symbols_per_file,
        } => {
            let langs: Vec<&str> = languages.split(',').collect();
            let files = gen_repo(&out, symbols, &langs, cluster_ratio, seed, symbols_per_file)?;
            println!(
                "generated {symbols} symbols across {files} files in {}",
                out.display()
            );
        }
        Cmd::Run { repo, queries } => {
            let r = run_bench(&repo, queries)?;
            println!(
                "symbols={} files={}\n\
                 full index     {:8.3}s\n\
                 incremental    {:8.3}s\n\
                 spot p50/p99/max ({} queries)  {:7.1} / {:7.1} / {:7.1} ms\n\
                 clusters={} in {:8.3}s\n\
                 db size        {:8.1} MiB",
                r.symbols,
                r.files,
                r.full_index_secs,
                r.incremental_index_secs,
                r.spot_queries,
                r.spot_p50_ms,
                r.spot_p99_ms,
                r.spot_max_ms,
                r.clusters,
                r.cluster_secs,
                r.db_bytes as f64 / (1024.0 * 1024.0),
            );
            println!("JSON: {}", serde_json::to_string(&r)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_deterministic_and_sized() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        for dir in [a.path(), b.path()] {
            gen_repo(dir, 300, &["rust", "kotlin", "swift"], 0.3, 7, 100).unwrap();
        }
        // Same seed ⇒ identical content.
        for (lang, ext) in [("rust", "rs"), ("kotlin", "kt"), ("swift", "swift")] {
            let fa = std::fs::read(
                a.path()
                    .join("src")
                    .join(lang)
                    .join("mod0000")
                    .with_extension(ext),
            )
            .unwrap_or_else(|_| {
                std::fs::read(
                    a.path()
                        .join("src")
                        .join(lang)
                        .join(format!("mod0000.{ext}")),
                )
                .unwrap()
            });
            let fb = std::fs::read(
                b.path()
                    .join("src")
                    .join(lang)
                    .join(format!("mod0000.{ext}")),
            )
            .unwrap();
            assert_eq!(fa, fb, "{lang} must be deterministic");
        }
        // ~300 functions across the tree.
        let mut fns = 0;
        for entry in walkdir(a.path()) {
            let s = std::fs::read_to_string(entry).unwrap();
            fns += s.matches("pub fn ").count()
                + s.matches("fun f").count()
                + s.matches("func f").count();
        }
        assert!(fns >= 300, "expected ≥300 functions, got {fns}");
    }

    /// Minimal recursive walk (no extra deps for a dev tool).
    fn walkdir(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push(p);
                }
            }
        }
        out
    }

    #[test]
    fn bench_run_measures_a_small_repo() {
        let dir = tempfile::tempdir().unwrap();
        gen_repo(dir.path(), 60, &["rust"], 0.3, 3, 30).unwrap();
        let r = run_bench(dir.path(), 10).unwrap();
        assert!(r.symbols >= 60, "report: {r:?}");
        assert!(r.full_index_secs > 0.0);
        assert!(r.spot_p99_ms > 0.0);
        assert!(r.db_bytes > 0);
    }
}
