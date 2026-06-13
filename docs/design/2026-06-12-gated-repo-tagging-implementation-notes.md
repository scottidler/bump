# Implementation Notes: Gated-Repo Tagging

Running, append-only record of how the implementation interprets or diverges from
`2026-06-12-gated-repo-tagging.md`. One section per phase.

## Phase 1: github module

### Design decisions
- `gh_command` token source — `src/github.rs:token_path` — the design says model on
  `gx/src/github.rs`, but `gx` reads the token path from a `Config` struct that `bump`
  does not have. Rather than import a whole config system (scope creep), `gh_command`
  reads the per-org token directly from `$XDG_CONFIG_HOME/github/tokens/{org}` (falling
  back to `$HOME/.config/...`), which is exactly `gx`'s *default* template. Ambient
  `gh auth` fallback with a `debug!` note is preserved.
- Rule-type extraction via `gh ... --jq '.[].type'` — `src/github.rs:probe_rulesets` —
  the design's "no new crates" constraint plus the gx precedent of using `--jq` means we
  let `gh`'s built-in jq pull the `type` fields out of the rulesets array rather than add
  `serde_json` to parse it in-process.
- Classic-protection 404 detection — `src/github.rs:probe_classic` — `gh api` exits
  non-zero on 404 with `Not Found (HTTP 404)` on stderr; we treat that exact shape as
  "clear" and any other non-success as `Unknown`. 404 is not in the retryable-error set,
  so it returns on the first attempt.
- Slug host restriction — `src/github.rs:parse_slug` — only `github.com` remotes are
  recognized; any other host (enterprise, gitlab, etc.) yields `Unknown("not a GitHub
  remote")`, matching the design's stated scope (scottidler + tatari-tv, both github.com).
- `BUMP_GATES_PROBE` encoding — `ungated` | `gated[:type1,type2]` | `unknown:reason`.
  Commas separate rule types; this is an internal test/scripted env seam, not a CLI flag,
  so the no-comma CLI convention does not apply.

### Deviations
- Test placement — tests live in an inline `#[cfg(test)] mod tests` block in
  `src/github.rs`, not a separate `src/github/tests.rs`. The global `rust.md` rule prefers
  separate test files, but every existing file in this repo (`cli.rs`, `git.rs`,
  `main.rs`, `version.rs`) uses the inline form; matching the repo's own idiom wins here
  for consistency. Extracting all test modules is a separate tree-wide pass, out of scope.

### Tradeoffs
- `detect()` returns `Gate` (infallible), never `Result` — probe failures collapse to
  `Gate::Unknown(reason)`. This matches the design's "warn and proceed as ungated" policy:
  the caller never has to distinguish a transport error from a verdict; `Unknown` carries
  the reason for the warning.

### Open questions
- None.
