# Ward

> **Ward off AI slop.**
>
> Guardrails and a verification layer for AI agent coding. Ward does not replace
> Git, does not own truth, and never rewrites code unattended — it does three
> small things and does them well:

1. **Spot** — checks for existing similar implementations *before* the agent
   writes code (pre-generation duplicate interception).
2. **Replay** — deterministic, symbol-level change summaries where every factual
   claim is anchored to `file:line` (semantic review summaries).
3. **Catch + Form Check** — deterministic verification in CI: real test runs in
   a sandbox and machine-checkable spec assertions (spec drift guard).

## Status

Early implementation. The full design lives in
[docs/ward-tech-spec-v0.6.1.md](docs/ward-tech-spec-v0.6.1.md).

## Design principles (the seven iron laws)

| # | Law | Meaning |
| :--- | :--- | :--- |
| P1 | Git is the only source of truth | Everything Ward produces is rebuildable from `git + working tree`; delete `.ward/` and you lose speed, not correctness |
| P2 | Advisory, not Authority | Ward advises, measures, reports. Every write goes through `git commit / PR`. Fail-open on code *content*; flow constraints require a human-reviewed spec |
| P3 | Fail-open (inner loop) | Stale index, parse errors, daemon down → checks degrade to "skip and record", never block |
| P4 | Reuse first | tree-sitter, SQLite, jscpd, per-language API-compat tools; self-built only for the two differentiating modules |
| P5 | Measure first | Every module ships with metrics and graduation thresholds, or it ships not at all |
| P6 | Deterministic backstop | Tests/lint/types/contract assertions run deterministically in CI; LLMs only narrate and rank, never adjudicate |
| P7 | Two loops, two postures | Inner loop fail-open, outer loop (CI) fail-closed: assertion failure = red, `unknown` is never green |

## Workspace layout

```
crates/
  ward-core/   # indexing, fingerprints, search, diff, storage (library)
  ward-cli/    # user-facing CLI (index / spot / replay / verify / form-check)
  ward-mcp/    # MCP server daemon exposing spot / replay / catch_run / form_check
hooks/         # Claude Code PreToolUse/PostToolUse hook scripts
examples/      # example spec files
docs/          # design documents
```

## Quick start

```bash
cargo build --release
./target/release/ward --help
```

## License

MIT — see [LICENSE](LICENSE).
