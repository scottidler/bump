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

## Phase 2: gated refusal in the default path

### Design decisions
- Refusal surfaced via `bail!` — `src/main.rs:process_directory` — the gated recipe is
  returned as the `eyre` error so `main`'s existing `eprintln!("Error: {:#}", e)` handler
  renders it as one coherent message (the message is worded so the `Error:` prefix reads
  naturally). This reuses the existing error/exit path rather than printing-then-bailing
  (which would double-print).
- Repo label without a re-probe — `src/github.rs:repo_label` / `local_default_branch` —
  the refusal message names "'branch' on owner/repo" using local git only (remote URL +
  `refs/remotes/origin/HEAD` symref), no extra network call. `default_branch` was
  refactored to share `local_default_branch`.
- Gate check placement — runs at step 1b, before project detection and any file/git
  mutation, so refusal is guaranteed to precede mutation (verified by
  `gated_refusal_aborts_before_mutation`).

### Deviations
- Aggregate exit code — `src/main.rs:main` — changed the multi-directory exit condition
  from `failures > 0 && successes == 0` to `failures > 0`. The design's Phase 2 calls out
  "aggregate exit code" and the risk table wants gated refusals in a batch to "fail
  loudly"; under the old condition a gated refusal was masked by any sibling success. This
  also makes any other per-repo error fail the batch, which is the more correct behavior.

### Tradeoffs
- The Unknown-policy warning and the refusal recipe both reference flags that land in
  later phases (`--gates`, `--no-tag`, `--tag-only`). Since all five phases ship in one
  push, the referenced flags exist by release time; the intermediate commits name them
  ahead of their implementation by design (the recipe is the whole point of the feature).

### Open questions
- None.

## Phase 3: --no-tag

### Design decisions
- `create_tag` gating — `src/main.rs:process_directory` — threaded a single
  `let create_tag = !cli.no_tag` through both workflow branches (standard and clean-tree)
  and the dry-run block; each `git::create_tag` call site is guarded by it, with distinct
  "no tag" output naming the version and pointing at `--tag-only` for later.
- `--no-tag` skips the gate probe entirely — there is no tag to orphan, so the probe is
  pointless and would only slow the PR inner loop (matches the design's performance note).
- The `--no-tag` flag is added WITHOUT the design's `conflicts_with_all = ["tag_only"]`
  because `tag_only` does not exist until Phase 4; clap panics at command-build time if a
  conflict references an unknown arg id. The conflict is added in Phase 4 when both flags
  coexist.

### Deviations
- None.

### Tradeoffs
- `no_tag_bumps_file_without_tagging` passes `-a` — a realistic `--no-tag` run commits
  code changes alongside the version bump, so the staged set is not version-files-only and
  `determine_commit_message` would otherwise open `$EDITOR` (which hangs headless). `-a`
  (automatic message) reflects how the flag is actually used and keeps the test
  non-interactive.

### Open questions
- The clean-tree tagging path still prints `Run: git push && git push --tags`, which
  conflicts with the git.md rule (never `git push --tags`; push the tag by explicit name).
  Left unchanged here (out of --no-tag scope); corrected in Phase 5 alongside the README /
  after_help push recipes.

## Phase 4: --tag-only

### Design decisions
- `compare_head_to_remote` returns a `HeadRemote` enum (`Equal`/`Ahead`/`Behind`/
  `Diverged`) — `src/git.rs` — rather than the design's literal `head_equals_remote`
  boolean. The ladder needs to emit *distinct* ahead/behind/diverged errors (design step 3
  explicitly: "behind and ahead are both errors with distinct messages"), which a bool
  cannot carry.
- Extra git primitives beyond the design's four — added `current_branch`, `head_sha`,
  `tag_sha`, plus private `rev_parse`/`is_ancestor` helpers — in addition to the specified
  `fetch_branch`, `remote_default_branch`, `remote_tag_sha`. The ladder's branch check and
  idempotency/conflict comparisons need them.
- `remote_tag_sha` dereferences annotated tags via the `^{}` peeled line — `src/git.rs` —
  so the comparison is commit-vs-commit (an annotated tag's own object SHA is never equal
  to HEAD's commit SHA; comparing the peeled commit is what makes idempotency correct).
- `--tag-only` does not invoke the gate refusal — `process_directory` dispatches to
  `tag_only` before the gate block. The exact `HEAD == origin/<default>` equality check is
  the real safety here (the design: the probe would only "label its report, never refuse").
- Tag message is `Release vX.Y.Z` — the merged commit already has its own message; the
  annotated-tag message just records the release.

### Deviations
- None beyond the enum rename noted above.

### Tradeoffs
- `remote_default_branch` (git.rs) overlaps `github::local_default_branch` — both read the
  `origin/HEAD` symref. Kept separate: git.rs owns the tag-only ladder's git plumbing,
  github.rs owns gate probing. Unifying would couple the two modules for ~5 lines.
- Tests stand up a real bare `origin` and exercise fetch/ls-remote/reset against it (no
  network, all filesystem), covering every ladder rung in isolation: dirty tree, wrong
  branch, ahead, behind, local-idempotent, remote-conflict, generic-no-version, happy path.

### Open questions
- None.
