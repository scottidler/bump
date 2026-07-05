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
