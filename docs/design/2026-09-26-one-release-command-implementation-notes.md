# Implementation Notes: one release command

Running record of how the implementation diverges from or interprets the design
doc (`2026-09-26-one-release-command.md`). Append-only, one section per phase.
Phase 0 was observation-only and its findings are recorded directly in the design
doc's Phase 0 bullets, not here.

## Phase 1: git helpers and the amend fix

### Design decisions
- `is_head_pushed` (`src/git.rs:is_head_pushed`): added the remote-containment check
  (`git branch -r --contains HEAD`) BEFORE the existing `@{u}` check, exactly as the
  doc's Architecture section and Phase 0's observation specify. A branch cut with
  `--no-track` (or any branch whose upstream was never set) now still reads as pushed
  when its tip is already reachable from an `origin/*` ref, closing the false-negative
  that made `process_directory`'s clean-tree path amend a commit that had already
  landed on origin.
- `commit_subjects`, `changed_files`, `checkout_new_tracking`, `worktree_for_branch`
  (`src/git.rs`) added per the doc's Architecture bullet, each with its own tests.
  None has a production caller yet (their callers land in Phases 3-5 per the doc's own
  phase table), so each carries `#[allow(dead_code)]` with a comment naming the phase
  that wires it in -- otherwise `otto ci`'s `clippy -D warnings` fails on introducing
  forward-looking infrastructure a phase ahead of its caller, which is inherent to
  shipping git helpers and their consumer in separate phases.
- `Cli.never_amend` (`src/cli.rs`): added as `#[arg(skip)]` on the top-level `Cli`
  struct (not `ReleaseArgs`), because both amend sites it gates live in
  `process_directory`, which takes `&Cli`, not `&ReleaseArgs`. Found and used its
  first real caller in the SAME phase: `src/release.rs:version_commit` builds a `Cli`
  by hand to reuse `process_directory`'s `--no-tag` path for the gated verb's own
  version commit; it now sets `never_amend: true` there, since the release verb's own
  commit must never fall into the amend fallback regardless of what
  `is_head_pushed` reports. Both amend decision sites in `process_directory`
  (`src/main.rs`, the dry-run text branch and the real clean-tree-workflow branch) now
  read `cli.never_amend || git::is_head_pushed(dir)?`.

### Deviations
- None. The doc's exact function names, `is_head_pushed` check ordering, and
  `Cli.never_amend` mechanism were implementable as specified; the `prototype-one-
  release-command` branch's `src/git.rs` (commit `f15add3`) matched the current doc
  closely enough that its four new helper bodies were reused near-verbatim (adjusted
  for this repo's current doc comments/imports), not cherry-picked wholesale.

### Tradeoffs
- `#[allow(dead_code)]` on the four new helpers vs. wiring a stub caller into this
  phase just to satisfy clippy: chose the explicit allow with a phase-pointer comment.
  A fake caller would be exactly the kind of "stub to look complete" the phase-
  implementer rules forbid; the allow is honest about the seam being unfinished until
  its phase.

### Open questions
- None.
