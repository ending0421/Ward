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

One Rust binary, two postures: a local **MCP daemon** (inner loop, fail-open,
never blocks) and a **CLI for CI** (outer loop, fail-closed: assertion
failure is red, `unknown` is never green).

The full design — seven iron laws, four-layer fingerprints, failure-mode
catalog, metrics with graduation thresholds, competitive analysis — lives in
[docs/ward-tech-spec-v0.6.1.md](docs/ward-tech-spec-v0.6.1.md).

## Status: Phase 0/1 core complete

| Spec phase | Scope | Status |
| :--- | :--- | :--- |
| Phase 0 | M1 Spot prototype on Rust (self-dogfood) | ✅ implemented |
| Phase 1 | L2 simhash + feedback loop + M2 deterministic layer | ✅ core implemented (block-level fingerprints & embeddings deferred) |
| Phase 2 | M3 sandbox adjudication + M4 assertions + LLM narration | 🟡 deterministic parts implemented; LLM narration not included (structured fallback only, per F6) |
| Phase 3 | Kotlin/Swift/Java/OC grammars, M5/M6 | ⏳ grammar registry is ready; grammars not yet compiled in (fail-open skip) |

## Quick start

```bash
cargo build --release            # single static binary

# initialize a starter config
./target/release/ward init --repo .

# build the index (The Rack, .ward/index.db — deletable, rebuildable)
./target/release/ward index --repo .

# pre-generation duplicate check (M1)
./target/release/ward spot --repo . \
  --intent "防抖函数，支持 leading/trailing" \
  --signature "pub fn debounce(f: &dyn Fn(u64), ms: u64) -> u8"

# deterministic change summary between two commits (M2)
./target/release/ward replay HEAD~3 HEAD --repo .

# inner-loop lint/type precheck (M3, no Docker)
./target/release/ward catch-run --repo .

# outer-loop adjudication in a Docker sandbox (M3; 'unknown' without Docker)
./target/release/ward verify --full --repo .

# evaluate a task spec's assertions (M4, inner-loop semantics)
./target/release/ward form-check --spec specs/2026-0813-debounce.md --repo .

# record the agent's self-reported action for an advisory (M1 feedback loop)
./target/release/ward action <advisory_id> accepted
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

The daemon exposes `spot`, `replay`, `catch_run`, `verify_full`,
`form_check`, and `spot_action` over stdio MCP, using the official Rust MCP
SDK ([rmcp](https://crates.io/crates/rmcp)). All tools are fail-open and
report structured results — a failure is an answer, never a broken session.

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
| L3 | embeddings (not yet compiled in) | semantic clones — recall only, never thresholds |

Pipeline: L1 equality → BM25 recall → L2 simhash ranking → thresholded
grades. **Strong grades require fingerprint evidence**; text-only matches are
capped at `weak` by construction.

## How Replay works (deterministic, anti-hallucination)

`git diff base..head` → tree-sitter parse of both versions → symbol alignment
→ classification (`added / removed / signature_changed / body_changed /
doc_only / moved`) → 1-hop impact via static mention edges (**lower-bound
estimates** — reports say "at least N") → risk markers (public API changes,
high fan-in, tests not updated, suspected duplicates). The LLM narration
layer is deliberately *not* in this codebase: every line of output here is a
deterministic fact with a `path:line` anchor (the F6 structured fallback).

## Repository layout

```
crates/
  ward-core/   # engine: config, fingerprints, indexer, search, diff, spec, verify, store
  ward-cli/    # the `ward` binary (init/index/spot/replay/catch-run/verify/form-check/action)
  ward-mcp/    # MCP daemon (stdio, official Rust SDK)
hooks/         # Claude Code PreToolUse/PostToolUse scripts
examples/      # example spec file + Claude Code settings + starter config
.github/       # CI: fmt/clippy/test, self-dogfood spot, jscpd duplication baseline
docs/          # the design spec (v0.6.1)
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

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## License

MIT — see [LICENSE](LICENSE).
