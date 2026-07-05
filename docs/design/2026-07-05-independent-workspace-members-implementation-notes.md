# Implementation notes: independent workspace members

Design doc: `docs/design/2026-07-05-independent-workspace-members.md`

The design has no explicit numbered phases (single cohesive change), so it was
implemented as one phase: the `--skip-member` flag, the `validate_project` filter,
the terminal print, tests, and this note.

## Phase 1: --skip-member

### Design decisions
- `--skip-member` flag shape (`src/cli.rs`) — declared as
  `#[arg(long = "skip-member", value_name = "NAME", num_args = 1.., action = clap::ArgAction::Append)]`
  so it accepts BOTH space-separated (`--skip-member a b`) and repeated
  (`--skip-member a --skip-member b`) forms, satisfying the CLI rule and the doc's
  acceptance criterion "CLI parse (space-separated, repeated)".
- Extracted `skip_message(&IndependentVersionMember) -> String` (`src/main.rs`) so the
  exact terminal wording is unit-testable, while `validate_project` still emits it with
  `println!` (NOT `info!`) exactly as the design mandates (logging is file-routed and
  would be invisible).
- Check ordering in `validate_project` (`src/main.rs`) — the stale/unmatched
  `--skip-member` check runs BEFORE the unhandled-independent-member check. Both are
  fail-closed errors; reporting the stale flag first gives the operator the most direct
  fix when a skip name is wrong.
- The unmatched check also naturally covers "a `--skip-member` on a workspace where that
  member already inherits `version.workspace`": such a name matches no independent member,
  so it is unmatched and aborts. No separate code path needed.

### Deviations
- None. `--tag-only` is untouched (returns before the guard); its call site was not
  modified. Only `validate_project`, its signature, and the single call site at
  `process_directory` changed.

### Tradeoffs
- `num_args = 1..` (space-separated support) vs. plain `Vec<String>` (repeated-only).
  Chose `num_args = 1..` to honor the doc's explicit space-separated requirement.
  **Caveat:** because `directories: Vec<PathBuf>` is a trailing variadic positional,
  `bump --skip-member claude-pricing ./some-dir` greedily consumes `./some-dir` into
  `skip_member`. The realistic invocations are unaffected: run `bump` in-place (no
  positional), or let another flag terminate the value list
  (`--skip-member claude-pricing --no-tag`, `... -a`). If both `--skip-member` and an
  explicit directory are needed, put the directory first or terminate with `--`.

### Open questions
- None.

## Post-audit fixes (review-panel Implementation Audit, commit b71b630)

The Architect approved with zero findings; the Staff Engineer surfaced two real
cheap-wins (both fail-closed / disclosure gaps, neither a correctness defect). Both
were fixed rather than merely disclosed:

- **Non-Rust `--skip-member` no longer rots silently.** `validate_project`
  (`src/main.rs`) previously returned `Ok(())` for a non-Rust project *before* looking
  at `skip_members`, so `bump --skip-member typo` on a Python/generic repo was a silent
  no-op — contradicting the CLI help, the doc's "a stale skip flag can't rot silently in
  CI" goal, and the fail-closed rule. Now a non-empty `skip_members` on a non-Rust
  project `bail!`s. Covered by `validate_skip_member_on_non_rust_aborts`.
- **Terminal visibility is now a biting test.** The `println!` skip announcement had no
  test that would catch a `println!` -> `info!` regression (the exact failure this
  feature exists to prevent). Added `tests/skip_member.rs`, an end-to-end test that runs
  the compiled binary and asserts the skip line appears on real stdout. The earlier
  "Deviations: None" overstated coverage on this axis; this note and the new test correct
  it.

### Decisions reaffirmed by the audit (no change)
- `num_args = 1..` (space-separated) is kept: `rules/cli.md` mandates space-separated
  list flags, so repeated-only would violate the house rule. The trailing-positional
  greediness fails closed (a swallowed directory becomes a stale skip name that aborts
  validation before any mutation). The behavior is now an asserted contract:
  `test_cli_skip_member_swallows_trailing_positional` (`src/cli.rs`).
- Stale-before-unhandled check ordering, and printing the skip under `--dry-run`, were
  both confirmed correct.
