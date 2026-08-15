# Ward

> **Ward off AI slop.**
>
> Ward is a **guardrail and verification layer for AI agent coding**. It does
> not replace Git, does not own truth, and never rewrites code unattended. It
> does three small things and does them well:

1. **Spot** — checks for existing similar implementations *before* the agent
   writes code (pre-generation duplicate interception).
2. **Replay** — deterministic, symbol-level change summaries where every
   factual claim is anchored to `path:line` (semantic review summaries).
3. **Catch + Form Check** — deterministic verification: real test runs in a
   sandbox, and machine-checkable spec assertions guarding intent drift.

Two Rust binaries, two postures: a local **MCP daemon** (`ward-mcp`,
inner loop, fail-open, never blocks) and a **CLI for CI** (`ward`, outer
loop, fail-closed: assertion failure is red, `unknown` is never green).

The full design — seven iron laws, four-layer fingerprints, failure-mode
catalog, metrics with graduation thresholds, competitive analysis — lives in
[docs/ward-tech-spec-v0.6.1.md](docs/ward-tech-spec-v0.6.1.md).

## Status: Phase 0–3 implemented

| Spec phase | Scope | Status |
| :--- | :--- | :--- |
| Phase 0 | M1 Spot prototype on Rust (self-dogfood) | ✅ implemented |
| Phase 1 | L2 simhash + block-level fingerprints + feedback loop + M2 deterministic layer | ✅ implemented |
| Phase 2 | M3 sandbox adjudication + M4 assertions + M2 narration (anchor-validated, F6 fallback) + M4-b intent drift + api_compat orchestration | ✅ implemented (LLM via `WARD_LLM_URL`; L3 hashing embedder with pluggable provider trait; Rust api_compat via cargo-semver-checks) |
| Phase 3 | Five grammars + LanguageSpec (index **and** spot query; signature language auto-detected, `--language` override), M5 context cards, M6 duplicate clustering | ✅ implemented |

## One-line install (Claude Code / Codex / Cursor)

```bash
curl -fsSL https://raw.githubusercontent.com/ending0421/Ward/master/scripts/install.sh | sh
```

What it does: downloads the official release binary for your platform,
verifies it against SHA256SUMS.txt, links `ward`/`ward-mcp` into
`~/.local/bin`, and registers the MCP server with the tools it finds:

| Tool | Registration | Verification |
| :--- | :--- | :--- |
| Claude Code | `claude mcp add --scope user ward` | `claude mcp list` → `ward ✔ Connected` |
| Codex | `[mcp_servers.ward]` in `~/.codex/config.toml` | `codex mcp list` |
| Cursor | project `.cursor/mcp.json` (project mode) or UI steps (global) | Settings → MCP |

Options: `--project` (project scope: `.mcp.json`, `.cursor/mcp.json`,
`.claude/settings.json` hooks, `.codex/config.toml`), `--no-mcp` (binaries
only), `--uninstall` (removes binaries and registrations). Pin a version
with `VERSION=v0.1.0 sh scripts/install.sh`.

## Quick start

```bash
cargo build --release            # ward + ward-mcp binaries

# initialize a starter config
./target/release/ward init --repo .

# build the index (The Rack, .ward/index.db — deletable, rebuildable)
./target/release/ward index --repo .

# pre-generation duplicate check (M1)
./target/release/ward spot --repo . \
  --intent "防抖函数，支持 leading/trailing" \
  --signature "pub fn debounce(f: &dyn Fn(u64), ms: u64) -> u8"

# same, against a Kotlin codebase — the signature language is auto-detected
# (all five grammars are compiled in; override with --language kotlin)
./target/release/ward spot --repo . \
  --intent "防抖函数，支持 leading/trailing" \
  --signature "fun debounce(f: (Long) -> Unit, ms: Long): Unit"

# deterministic change summary between two commits (M2)
./target/release/ward replay HEAD~3 HEAD --repo .

# inner-loop lint/type precheck (M3, no Docker)
./target/release/ward catch-run --repo .

# outer-loop adjudication in a Docker sandbox (M3; 'unknown' without Docker)
./target/release/ward verify --full --repo .

# evaluate a task spec's assertions (M4; add --ci for the fail-closed outer
# loop — exit 1 on any `fail`, exit 2 on any `unknown`)
./target/release/ward form-check --spec specs/2026-08-gov-data.md --repo .
# (an example spec ships at examples/specs/2026-0813-debounce.md)

# record the agent's self-reported action for an advisory (M1 feedback loop)
./target/release/ward action <advisory_id> accepted
```

### Configuration (`.ward/config.toml`)

```toml
# Languages Ward indexes and matches. All five grammars are compiled in;
# this list restricts the set (case-insensitive names; unknown names are
# ignored with a warning — fail-open). Defaults to all five.
languages = ["rust", "kotlin", "swift", "java", "objc"]

suppress = ["vendor/", "generated"]     # hide paths from Spot advisories
top_k = 5                               # matches per advisory

[thresholds]
strong = 0.92   # initial value — recalibrate weekly against the golden set
weak = 0.80

[clusters]
exclude_tests = true    # M6: skip #[cfg(test)] mod tests / tests/ near-dupes

[lint]                  # catch-run inner-loop precheck (empty command disables)
command = "cargo check --quiet"
timeout_secs = 120

[sandbox]               # verify --full outer-loop adjudication (Docker)
verify_command = "cargo test --quiet"
image = "rust:1-bookworm"
memory = "2g"
```

### Connect as an MCP server (Claude Code)

```jsonc
// ~/.claude.json  (or .mcp.json in the repo root)
{
  "mcpServers": {
    "ward": {
      "command": "ward-mcp",
      "args": []
    }
  }
}
```

The daemon exposes 10 tools over stdio MCP, using the official Rust MCP
SDK ([rmcp](https://crates.io/crates/rmcp)):

| Tool | Purpose |
| :--- | :--- |
| `spot` | Pre-generation duplicate check (M1); `language` param overrides signature auto-detection |
| `spot_action` | Record the agent's disposition for an advisory (feedback loop) |
| `replay` | Deterministic symbol-level change summary between two commits (M2) |
| `catch_run` | Inner-loop lint/type precheck (M3, no Docker) |
| `verify_full` | Outer-loop adjudication in a Docker sandbox (M3) |
| `form_check` | Evaluate a task spec's assertions (M4) |
| `intent_check` | Intent-drift comparison of a requirement against a diff (M4-b) |
| `compat_check` | Public-API compatibility between two revisions |
| `context_card` | Read a symbol's context card: fingerprint layers, mentions, change history |
| `clusters` | Duplicate clusters at a similarity threshold (M6) |

All tools are fail-open and report structured results — a failure is an
answer, never a broken session.

### Hooks (Claude Code)

`hooks/` contains the PreToolUse/PostToolUse scripts described in the spec's
harness matrix (§3-M1): PostToolUse refreshes the index after writes;
PreToolUse uses **deny-with-reason** for large insertions because Claude Code
cannot inject context from PreToolUse
([#15664](https://github.com/anthropics/claude-code/issues/15664),
[#19432](https://github.com/anthropics/claude-code/issues/19432)). See
[examples/claude-code-settings.json](examples/claude-code-settings.json).

## How Spot works (the four-layer fingerprint system)

| Layer | Fingerprint | Captures |
| :--- | :--- | :--- |
| L0 | `body_hash` — blake3 of raw source | exact clones |
| L1 | `struct_hash` — hash of the canonical form (identifiers→`ID`, literals→`LIT`, comments dropped) | clones + pure rename + literal substitution |
| L2 | `sig_simhash` / `simhash` — Charikar simhash over subtree features (DECKARD-inspired variant) | structural near-duplicates; signature-shaped queries align against the signature simhash |
| Block | sliding statement-window simhashes inside function bodies | in-function duplication (the granularity symbol-level fingerprints cannot see) |
| L3 | embeddings — hashing embedder (compiled in, recall supplement only); learned semantic embeddings not yet compiled in | semantic clones — recall only, never thresholds |

Pipeline: L1 equality → BM25 recall → L2 simhash ranking → thresholded
grades. **Strong grades require fingerprint evidence**; text-only matches are
capped at `weak` by construction.

## More commands

```bash
# one-page context card: definition, callers (lower bound), tests, config refs (M5)
./target/release/ward card simhash --repo .

# offline duplicate clustering for the consolidation workflow (M6)
./target/release/ward clusters --threshold 0.92 --repo .

# LLM narration over Replay (M2): every sentence anchor-validated, F6 fallback
WARD_LLM_URL=https://api.example.com/v1/chat/completions \
  ./target/release/ward replay HEAD~3 HEAD --narrate --repo .

# API/ABI compatibility adjudication (M4 outer loop; Rust = cargo-semver-checks)
./target/release/ward compat-check --base HEAD^ --repo .

# Soft intent-drift check (M4-b; LLM partition, "not executed" without WARD_LLM_URL)
./target/release/ward intent-check --requirement "实现防抖函数" --repo .
```

### Governance loop (data & calibration)

Spot advisories feed a feedback loop that calibrates Ward's own
thresholds — the only non-automatable step is a weekly human verdict on the
golden set (~5 min):

```bash
ward infer --repo .                 # objective channel: infer adoption
                                    # outcomes for pending advisories from
                                    # the next commit (post-commit hook)
ward setup-hooks --repo .           # install the git post-commit hook that
                                    # auto-runs `ward infer` (--remove to undo)
ward label next --repo .            # golden-set labeling: next unlabeled match
ward label set <advisory> <match> y # record a human verdict (y = relevant, n = not)
ward calibrate --repo .             # Wilson-interval threshold calibration
ward snapshot --repo .              # idempotent daily trend snapshot
ward stats --repo .                 # governance report: adoption, clusters,
                                    # constraint decay (also --json)
```

### Unattended operation & remote diagnostics

```bash
ward daemon --repo .                # background worker: watch→index,
                                    # poll→infer, daily snapshot, weekly metrics
ward service install --repo .       # launchd (macOS) / systemd (Linux) service;
                                    # --dry-run prints the unit; uninstall removes
ward doctor --repo . --bundle       # health probe, privacy-redacted portable bundle
ward report <advisory-id> --include-snippets   # full context dump for one advisory
ward issue --title "..." --bundle <bundle> --yes   # compose a GitHub issue
                                    # (default dry-run; nothing is posted without --yes)
```

## How Replay works (deterministic, anti-hallucination)

`git diff base..head` → tree-sitter parse of both versions → symbol alignment
→ classification (`added / removed / signature_changed / body_changed /
doc_only / moved`) → 1-hop impact via static mention edges (**lower-bound
estimates** — reports say "at least N") → risk markers (public API changes,
high fan-in, tests not updated, suspected duplicates). The structured list is
100% deterministic facts with `path:line` anchors. An optional LLM narration
layer (`--narrate`, requires `WARD_LLM_URL`) can add prose on top — every
sentence is anchor-validated against those facts, and any failure falls back
to the structured list (F6): the LLM narrates, never adjudicates.

## Repository layout

```
crates/
  ward-core/   # engine: lang, normalize, fingerprint, index, search, diff,
               # spec, verify, compat, infer, label, calibrate, stats, cluster,
               # context, doctor, report, daemon, store, config, embedding,
               # fresh, git, intent, llm, narrate
  ward-cli/    # the `ward` binary (23 subcommands: init/index/spot/replay/
               # catch-run/verify/form-check/compat-check/infer/label/calibrate/
               # snapshot/stats/daemon/service/doctor/report/issue/setup-hooks/
               # intent-check/card/clusters/action)
  ward-mcp/    # MCP daemon (stdio, 10 tools, official Rust SDK)
  ward-bench/  # F11 scale benchmark: synthetic-repo gen + timed engine runs
hooks/         # Claude Code PreToolUse/PostToolUse scripts
examples/      # example spec + Claude Code settings
specs/         # active task specs (form-check gate input)
scripts/       # install.sh, verify-meaningful.sh, gen-release-notes.sh
.github/       # CI: reusable quality gate, spec gate, 5-target release pipeline
docs/          # design spec (v0.6.1), usage guide, acceptance criteria,
               # curated release notes, metrics baseline
```

## Design principles (the seven iron laws)

| # | Law | Meaning |
| :--- | :--- | :--- |
| P1 | Git is the only source of truth | Everything is rebuildable from `git + working tree`; delete `.ward/` and you lose speed, not correctness |
| P2 | Advisory, not Authority | Ward advises, measures, reports. Writes go through git. Fail-open on code *content*; flow constraints require a human-reviewed spec |
| P3 | Fail-open (inner loop) | Stale index, parse errors, daemon down → "skip and record", never block |
| P4 | Reuse first | tree-sitter, SQLite (rusqlite), jscpd, rmcp; self-built only for the differentiating modules |
| P5 | Measure first | Metrics and graduation thresholds ship with the design (§9), not after |
| P6 | Deterministic backstop | Tests/lint/type/contract assertions run deterministically in CI; LLMs narrate, never adjudicate |
| P7 | Two loops, two postures | Inner loop fail-open, outer loop fail-closed: fail = red, `unknown` is never green |

## Releasing

The release pipeline (`.github/workflows/release.yml`) is **Release-Target
driven**:

1. **Trigger** — push a `v*` tag (canonical Release Target), or run the
   workflow manually (`workflow_dispatch` derives the tag from the workspace
   version in `Cargo.toml`).
2. **Quality gate** — the same reusable checks that block merges
   (fmt / clippy `-D warnings` / full test suite / coverage ≥85%) run before
   any artifact is produced.
3. **Version consistency** — on tag pushes, the tag must match the workspace
   version; mismatches fail the pipeline.
4. **Artifacts** — five targets: Linux x86_64 + aarch64 (cross), macOS
   x86_64 + aarch64, Windows x86_64. Each package is a tar.gz/zip containing
   `ward`, `ward-mcp`, LICENSE, README, hooks, examples and docs.
5. **Release Notes** — curated per release in
   `docs/release-notes/vX.Y.Z.md` (template: `_TEMPLATE.md`; highlights /
   breaking changes / migration / verification numbers / contributors),
   reviewed with the code; the pipeline **fails the release when the curated
   file is missing**, and appends the auto-generated grouped Full Changelog
   (`scripts/gen-release-notes.sh`).
6. **Publish** — `gh release create` with SHA256SUMS.txt, prerelease flag
   for `-rc`/`-beta` tags, and asset re-upload (--clobber) when re-running.

```bash
# Cut a release (tag must match the workspace version in Cargo.toml, and the
# curated notes file docs/release-notes/vX.Y.Z.md must already be merged):
git tag v0.4.0 && git push origin v0.4.0
# → quality gate → 5-target build → notes → GitHub Release
```

## Verifying Ward actually works

`scripts/verify-meaningful.sh` is the fail-closed self-check (11 checks, runs
in CI on every push): it plants known duplicate cases into a real git repo
and asserts the engine recalls the exact clone (L1), the copy-then-modify
(L2) and does **not** false-positive on unrelated code — then asserts the
adversarial semantics: F3 skips broken files, uncommitted edits mark
advisories `stale`, `verify --full` without a sandbox is `unknown` (never a
fake green), `must_pass` defers to CI, `api_compat` is `unknown` without its
tool (in CI, `form-check --ci` adjudicates it for real with
cargo-semver-checks), and `intent-check` without an LLM reports
"not executed".

```bash
scripts/verify-meaningful.sh $(command -v ward)
```

That proves correctness (levels 1–2). *Meaningfulness* (level 3) is a
workflow metric question and needs real usage data — see spec §9: baseline
vs intervention duplication rate (jscpd), review-time A/B, first-pass CI
rate, and constraint-decay measurements on dogfood repositories.

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo llvm-cov --workspace --summary-only   # coverage (cargo-llvm-cov, needs
                                            # llvm-tools-preview; CI gate ≥85% lines)
scripts/verify-meaningful.sh $(command -v ward)   # 11-check meaningfulness harness
cargo run --release -p ward-bench -- gen --symbols 10000   # F11 scale benchmark
cargo run --release -p ward-bench -- run --repo /tmp/ward-bench-repo
# numbers & thresholds: docs/BENCHMARKS.md
```

Dogfood discipline (see [AGENTS.md](AGENTS.md)): new functions go through
`ward spot` first, every change ends with `ward catch-run --repo .` and a
fresh `ward index --repo .`, and every received advisory gets a disposition
via `ward action`. Test coverage policy: core engine logic (fingerprints,
search, replay, spec, index, store) is exercised with positive and negative
cases per functional path — unit tests inside modules plus an end-to-end
suite that drives real temporary git repositories through the whole
pipeline.

## License

MIT — see [LICENSE](LICENSE).
