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

## Phase 2: github helpers

### Design decisions
- `persona_token_var` + `token_for_org` (`src/github.rs`): ported from the
  `prototype-one-release-command` branch near-verbatim. `gh_command` now calls
  `token_for_org` (token file -> `GITHUB_PAT_<ORG>` -> the persona var -> ambient `gh
  auth`) instead of only ever reading the token file, closing the gap where a
  work-org call with no token file present would silently go out under the ambient
  (possibly home) `gh auth` account. Tokens are read into the subprocess env only;
  `gh_command`/`token_for_org` log which SOURCE supplied a token (file, env var name,
  or "no token"), never the value itself.
- `CheckRuns`/`StatusState`/`check_runs_from_json`/`status_from_json`/`check_runs`
  (`src/github.rs`): `CheckRuns` gained the doc's `statuses: StatusState` field beyond
  the prototype's shape. `check_runs_from_json` now ALSO checks the check-runs
  payload's own `total_count` against the array actually returned and errors on
  truncation (`total_count > returned`), per the doc's explicit success criterion --
  the prototype draft didn't have this check. `status_from_json` is new: reads
  `total_count` FIRST and returns `StatusState::None` when it is zero, regardless of
  what the (misleading) `state` field says -- the doc's own observed case
  (`scottidler/bump`'s zero-status commits report `state: "pending"`). An
  unrecognized `state` value (anything but `success`/`pending`) maps to `Failure`,
  fail-closed rather than silently proceeding. `check_runs(path, sha)` runs both
  reads (`check-runs?per_page=100` and the legacy `/status` endpoint) and merges them
  into one `CheckRuns`; any non-success `gh api` result on either read is `Err`. No
  production caller yet (the `Ci` port and `wait_for_green` land in Phase 3), so
  `check_runs` carries `#[allow(dead_code)]` with a phase-pointer comment, same
  pattern Phase 1 used for its forward-looking git helpers.
- `git::create_tag` (`src/git.rs`) gained the `sha` parameter the doc specifies and
  now tags that EXPLICIT sha, never implicit HEAD. Updated every existing call site
  (four in `src/main.rs`'s `process_directory`/`tag_only`, three in
  `src/release.rs`'s `execute_release`/`execute_resume`/`finish`) to pass
  `git::head_sha(dir)?` -- in every one of those seven sites the tag was already being
  created immediately after a commit/amend/confirmed-push at HEAD, so this is a
  behavior-preserving signature change now, and the seam Phase 3's re-verify logic
  needs (tag the sha a fresh fetch just confirmed, not whatever HEAD drifts to).
- `git::push_branch` (`src/git.rs`) gained `--no-follow-tags`, per the doc's
  Architecture bullet.
- `git::remote_tip` and `git::manifest_version_at` (`src/git.rs`): both added per the
  doc, both `#[allow(dead_code)]` with a Phase 3 pointer (no caller yet -- the
  re-verify logic they serve is Phase 3's `wait_for_green`). `manifest_version_at`
  detects the ecosystem from the WORKING TREE (`lang::detect_project_type`) rather
  than from the historical commit itself -- a repo's manifest kind (Cargo.toml vs
  pyproject.toml vs package.json) does not change commit-to-commit within the life of
  a single release, so this is safe and avoids needing a second, tree-independent
  detection pass. It then runs `git show <sha>:<manifest>` and parses the blob with
  the same per-ecosystem logic `read_file_version` uses on disk, via the new
  `lang::read_version_from_content` dispatcher.
- `lang::read_version_from_content` (`src/lang.rs`) plus a `read_version_from_str`
  content-based twin in each of `cargo.rs`/`python.rs`/`node.rs`: a small refactor
  extracting the existing path-based `read_version` functions' parsing logic into a
  content-based function, with the path-based version now a thin
  `fs::read_to_string` + delegate wrapper. Needed because `manifest_version_at` reads
  a `git show` blob, never a file on disk, and re-implementing three ecosystems'
  parsing a second time in `git.rs` would fork the logic from `read_file_version`'s.
  All existing path-based tests for `read_version` untouched and still green.
- `config::Config` gained `ci: Option<CiDeclaration>` with `CiDeclaration`'s single
  variant `None` (bare-word `ci: none` in `bump.yml`). Deliberately did NOT add
  `deny_unknown_fields` to `CiDeclaration` itself -- serde's own "unknown variant"
  error on a one-variant enum already names the offending value loudly (verified by
  `load_ci_unknown_value_is_a_loud_error`), so no extra code was needed to satisfy
  "unknown values are a loud error" for this key.

### Deviations
- None from the doc's Phase 2 bullets. The doc's own hedge ("The `create_pr`
  signature change lands in Phase 3 with its caller, so this phase stays green on its
  own") held: `create_pr`'s signature is untouched here.

### Tradeoffs
- `manifest_version_at` detecting the ecosystem from the working tree vs. also trying
  to detect it from the historical commit: chose working-tree detection (see Design
  decisions) rather than adding a git-show-based `detect_project_type_at(sha)` --
  the doc's own function signature (`manifest_version_at(dir, sha)`, no separate
  ecosystem argument) implies the same assumption, and a repo changing its manifest
  ecosystem mid-release is not a case any row in the doc's state tables anticipates.
- Refactoring `read_version` into path-wrapper + content-function in three files vs.
  writing a fourth, independent parser in `git.rs`: chose the refactor -- the
  alternative forks the Cargo/PEP 621/poetry/package.json parsing rules (workspace
  inheritance, dynamic versions, top-level-only JSON) into a second implementation
  that would silently drift from `read_file_version`'s the next time either one
  changes.

### Open questions
- None.

## Phase 3: CI gate and PR by construction

### Design decisions
- `Ci` port + `GhCi` (`src/release.rs`); `Ports` gains `ci`; `release(dir, opts, pusher,
  installer, pr, ci)` and `finish(dir, opts, pusher, installer, ci)` take it;
  `main.rs::dispatch_release`/`dispatch_finish` pass `GhCi`. `github::check_runs` lost its
  `#[allow(dead_code)]`, as did `git::remote_tip`, `git::manifest_version_at` and
  `git::commit_subjects` (all now called). `changed_files`, `checkout_new_tracking` and
  `worktree_for_branch` keep theirs: their callers are Phases 4-5.
- Poll interval and appear window are methods on the `Ci` trait with the production
  constants as defaults (`CI_POLL_INTERVAL` 15s, `CI_APPEAR_WINDOW` 120s). Every test double
  overrides both to `Duration::ZERO` (the `zero_timing!` macro in `src/release/tests.rs`), so
  a zero-runs read is decided on the first poll and nothing sleeps; the timeout tests set
  `ReleaseOpts.ci_timeout` to zero. Elapsed time is still real `Instant` time in production.
  The port owns them because both describe the remote CI system (sane GitHub poll rate, how
  long GitHub takes to register a run), not the verb.
- One function creates and pushes every release tag: `gate_tag_and_push`. It runs
  `wait_for_green` on the sha, fetches `origin/<default>` fresh (`git::remote_tip`) and
  requires EQUALITY, restarts the gate on a moved tip whose committed manifest still carries
  the version (refuses on a different one, before any tag exists), checks the manifest at the
  sha (`git::manifest_version_at`), creates the tag on that explicit sha (keeping a local tag
  already there), fetches fresh again, and only then pushes. `execute_release`, the pending
  rows, and both finish tag arms (`Absent`, `LocalAtHead`, now one match arm) all call it.
- `wait_for_green`: `--no-ci-gate` skip -> committed `ci: none` skip (read with
  `config::load_at(dir, sha)`, i.e. `git show <sha>:bump.yml`) -> poll loop. Red = any failed
  run or `StatusState::Failure`; truncation and API errors come out of the port as `Err` and
  are wrapped "NO tag was created (fails closed)"; zero runs + `StatusState::None` after the
  appear window is decided by `git::has_workflows_at` (`git ls-tree <sha> .github/workflows`);
  incomplete runs or `StatusState::Pending` wait up to `ci_timeout`. The refusal never names
  `--no-ci-gate`.
- Pending version (`pending_version` -> `PendingCheck`): exactly the Data Model definition,
  classified after the behind/diverged refusal and before the ahead/equal split (ungated),
  and before the own-bump / inherited / fresh split (gated). `compute_target_tag` only runs
  on `NotPending`. `BelowLatest` refuses by name on either gate.
- `ReleaseState::Resume` became `UngatedPending { tag, version, default, ahead }` (the local
  tag at HEAD is detected inside `gate_tag_and_push` rather than carried as a flag). `ahead`
  pushes first; `resumed == !ahead`. An explicit level implying a different version is
  `UngatedLevelMismatch`. The named state landed here because the pending-version tests need
  it; Phase 4's bullet for it is now satisfied.
- Gated own bump = `git::version_line_changed(dir, "origin/<default>")`, a port of Gate D's
  diff test (`is_version_diff_line` ports its regex). An inherited pending version is
  `GatedInheritedPending` and commits `bump_version(manifest, level)` through the new
  `version_commit_to` (validate manifests, `lang::write_all`, stage, commit "Bump version to
  vX.Y.Z"), because `process_directory`'s `determine_version_action` refuses the
  manifest-above-tag state by design.
- `pr_title` / `pr_body` / `branch_slug` (the title guard's `title_slug`) are pure functions
  in `release.rs`; `execute_gated` builds title and body from `git::commit_subjects` after the
  version commit and calls `create_pr(dir, branch, default, title, body) -> url`
  (`github::create_pr` now passes `--head --base --title --body`, never `--fill`). The body's
  last line is `Release: rides this PR (vX.Y.Z)`. `GatedBadBranchName` is the Phase 3 slug
  precondition, classified before any mutation.
- Existing test fixture `setup_released` now pushes its tag. Under the pending-version
  definition a local-only tag means "a prior run died before pushing it" (the local-tag
  resume row), which is not what the fixture meant by "released".
- Break-the-code proofs, run by hand: replacing the `wait_for_green` call in
  `gate_tag_and_push` with a no-op makes `red_ci_leaves_no_tag_and_green_rerun_resumes_same_version`
  panic at the `expect_err("red CI must refuse")`; setting `never_amend: false` in
  `version_commit` makes `gated_pr_title_and_body_are_built_from_branch_and_commits` fail at
  `HEAD~1` (`left: "init"`, `right: "feat(core): add thing"`). Both restored, suite green.
- The force-move case of `tag_binds_to_verified_sha` uses a one-shot `reference-transaction`
  hook in the test clone (`core.hooksPath` set locally) that rewinds the bare origin's `main`
  the moment the local tag ref commits, so the production code path runs with no test seam.

### Deviations
- `ReleaseOpts` / `FinishOpts` gained `ci_gate` and `ci_timeout` now (the Data Model fields)
  because `wait_for_green` needs them; production dispatch passes `true` and
  `DEFAULT_CI_TIMEOUT` (1800s) until Phase 6 adds the `--no-ci-gate` / `--ci-timeout` flags.
- `ReleaseReport` gained `notice: Option<String>` beyond the doc's field list, so the
  success criterion "the pause output names the untagged v0.1.6" is asserted on data, not
  scraped stdout. Same printed text.
- The `Ci` trait carries `poll_interval` / `appear_window` default methods beyond the doc's
  one-method signature (see Design decisions). Same effect, the injectable seam the tests need.
- `finish` wiring of the gate (both tag arms) lands here per this phase's bullet; the Phase 5
  test named `finish_red_ci_leaves_no_tag` is left to Phase 5. This phase's coverage is
  `finish_gates_both_tag_arms_on_ci`.
- `wait_for_green_honors_committed_ci_none_only` exercises the `ci: none` read directly; the
  Phase 4 criterion test `zero_check_runs_refuses_with_workflows_and_proceeds_with_ci_none`
  is not written here.

### Tradeoffs
- Timing on the port vs. a separate clock/sleeper port: one fewer generic parameter
  threading through every execute function; real `Instant` time in production.
- Poll-count-free loop keyed on `Instant::elapsed` vs. counting polls: honors the doc's
  seconds semantics even when a `gh api` call is slow.
- A gate restart on a moved tip tags a sha that is not local HEAD; the verb prints the
  `git pull --ff-only` hint, and the install step still runs in the (now stale) working tree.
  Pulling automatically would mutate the operator's checkout mid-release; not done.
- `version_commit_to` duplicates the small write/stage/commit tail of `process_directory`
  instead of teaching `process_directory` an explicit-target mode: the inherited row is the
  only caller and `process_directory`'s version rules are shared with plain `bump`.

### Open questions
- `src/release.rs` is now 1638 lines and `src/release/tests.rs` 2068. Split (e.g. `ci.rs`
  for the gate, `pr.rs` for title/body/slug) before Phase 4 adds the standalone rows, or
  leave it?

## Phase 3 addendum: module split

Behavior-preserving decomposition (rules/rust.md 1500-line cap), between Phase 3 and 4.
`src/release.rs` stays the entry point; code moved verbatim into `src/release/ci.rs` (`Ci`,
`GhCi`, the CI constants, `CiGate`, `ci_gate`, `wait_for_green`), `pr.rs` (`Pr`, `GhPr`,
`pr_title`, `pr_body`, `branch_slug`), `tag.rs` (`TagTarget`, `gate_tag_and_push`,
`echo_tag_steps`, the re-verify helpers) and `finish.rs` (`finish` and its helpers).
`release.rs` re-exports the public surface (`Ci`, `GhCi`, `DEFAULT_CI_TIMEOUT`, `Pr`,
`GhPr`, `finish`), so `main.rs` is untouched. The only visibility widening is `pub(super)`
on `CiGate` (+ fields), `ci_gate`, `wait_for_green`, `TagTarget` (+ fields),
`gate_tag_and_push`, `echo_tag_steps` and `branch_slug`: the seams between the gate, tag
placement, and the state machine. Tests: `src/release/tests.rs` keeps the shared doubles,
harness and fixtures; test functions moved into `tests/{ungated,gated,install,finish,gate,pr}.rs`.
Test count unchanged (297 + 1). Line counts after: release.rs 1124, ci.rs 148, finish.rs 170,
pr.rs 103, tag.rs 150, tests.rs 585, tests/gate.rs 499 (largest test file).

## Phase 4: standalone, bump-only, bad branch name

### Design decisions
- `ReleaseOpts.standalone: Option<String>` (`src/release.rs`). `release()` refuses words that
  are empty or whitespace before classification, so an empty order mutates nothing
  (`standalone_with_empty_words_refuses`).
- New states: `UngatedStandalone`, `GatedStandalone { branch, default, target_tag }`,
  `GatedBumpOnlyBranch`, `Diverged`; `GatedFresh` gained `force`. `UngatedPending`,
  `GatedInheritedPending` and `GatedBadBranchName` landed in Phase 3 and are unchanged.
- `force` reaches `process_directory` through `version_commit(dir, level, force)`. It is true
  on exactly two paths: `UngatedStandalone` (`execute_release(.., force = true, ..)`) and a
  `GatedFresh` whose branch is bump-only and carries the order (`force: bump_only` in
  `classify_gated_feature`). A bump-only branch only gets that far with the order, so the
  gated side of "the standalone rows" is decided by the branch's diff, not by how the verb
  got there. That covers the fresh-cut `bump-vX-Y-Z` and a leftover empty one the same way.
- `src/release/standalone.rs` (new): `standalone_branch_name`, `is_bump_only_branch` plus the
  pure `bump_only(files, manifest_lines)`, `classify_gated_standalone`,
  `execute_gated_standalone`. The bump-only port matches `is_bump_only_ref`
  (`git-release-guard.sh:234-257`: root bump files only, at least one manifest line
  changed, and every changed manifest line a version line), with the doc's one difference:
  an empty file list (zero commits ahead, or commits that change nothing) IS bump-only.
- `classify_gated_standalone` picks the target the same way the new branch will classify
  once cut: a pending version inherited from the default bumps again from it
  (`bump_version(pending, level)`), otherwise `compute_target_tag`; below-latest and generic
  refuse first. `execute_gated_standalone` then checks out the existing branch
  (`git::local_branch_exists`) or cuts it with `checkout_new_tracking(branch, origin/<default>)`,
  runs `classify_gated_feature` on it and hands the resulting state back to `execute`. That
  is how "classify it like any feature branch from its actual diff" works: empty -> fresh
  with `force`, version line -> `GatedAlreadyBumped`, work -> fresh without `force`.
- `pr_body(subjects, tag, standalone)` quotes `Standalone release ordered by Scott:
  "<words>"` whenever the order is present, including on work-carrying gated branches.
  Ungated rows print the same line (`announce_order`) from `execute_release` and
  `execute_pending`.
- `git::changed_lines(dir, base, files)` (new) returns the `+`/`-` lines without the
  `+++`/`---` headers. `version_line_changed` now uses it too, so Gate D's test and the
  bump-only test read one diff helper. `git::local_branch_exists` is new too.
  `changed_files` and `checkout_new_tracking` lost their `#[allow(dead_code)]`.
- Refusal wording: `STANDALONE_DOOR` (`src/release.rs`) is the doc's sentence verbatim,
  appended to `Nothing` (ungated tagged, no order), `GatedDefaultClean` and
  `GatedBumpOnlyBranch`. The finish missed-bump refusal (`src/release/finish.rs:missed_bump`)
  carries the same door with the command spelled out (`run bump release on <default> with
  --standalone ...`), because it is not a re-run of `finish`.
- Behind vs Diverged: ungated classification returns `Diverged` (`git pull --rebase origin
  <default>`, then re-run). `tag_ladder`'s Diverged arm (`src/main.rs`) now names the rebase
  too. The gated default branch keeps Diverged under `GatedStranded` (the doc's "commits not
  on origin" row, unchanged).
- Fixture `setup_gated_already_bumped` now puts a work commit in front of the bump. Before,
  it was a version-only branch, which is now `GatedBumpOnlyBranch`. The level-mismatch test
  it serves is about a branch carrying work plus its own bump.
- Test `finish_missed_bump_refuses_with_branch_instruction` was renamed and inverted to
  `finish_missed_bump_refuses_naming_the_standalone_door`. It pinned the old "run bump
  release on a branch" text.
- Break-the-code checks, run by hand and then reverted: `force: false` in place of
  `force: bump_only` fails `gated_standalone_cuts_tracking_branch_bumps_and_quotes_scott` at
  its `expect` ("HEAD already has a tag"). Disabling the bump-only refusal fails
  `gated_bump_only_branch_refuses_without_standalone` and the version-only arm of
  `dep_bump_and_lockfile_only_branches_are_not_bump_only`.

### Deviations
- `--standalone` is NOT a CLI flag yet. `dispatch_release` passes `standalone: None` until
  Phase 6 adds the flags (Phase 6: "Flags per API Design"), the same pattern Phase 3 used for
  `ci_gate`/`ci_timeout`. Until then the standalone rows can only be reached from tests.
- `GatedFresh.force` is decided by the branch's diff (bump-only + order) instead of being
  flagged by the standalone path. Same effect, correct seam: see Design decisions.
- The doc's `UngatedPending { tag, default, ahead }` also carries `version` (from Phase 3).
- `dep_bump_and_lockfile_only_branches_are_not_bump_only` asserts on `classify()`
  (`GatedFresh { force: false }`, and a version-only control branch that comes out as
  `GatedBumpOnlyBranch`) instead of running the whole release. With a `Cargo.lock` present
  the version commit runs `cargo update -p`, which a fixture with a fake lockfile and no
  sources cannot satisfy. Classification is what the criterion names.
- `zero_check_runs_refuses_with_workflows_and_proceeds_with_ci_none` makes the untracked
  `bump.yml` ignored (`.git/info/exclude`). Without that, the verb refuses on the dirty tree
  before the CI gate ever runs. The test checks that the refusal is the CI one. The "notice"
  line is printed and not captured; the test asserts the proceed through the tag landing on
  origin at the `ci: none` commit.
- The finish missed-bump door wording lands here, not in Phase 5. The doc's "Refusal
  wording" paragraph groups it with the two door rows, and no Phase 5 bullet claims it.

### Tradeoffs
- `execute_gated_standalone` calls back into `classify_gated_feature` + `execute` vs. a
  dedicated standalone executor: reusing them means the existing-branch cases can't drift
  from ordinary feature-branch behavior. The recursion stops after one level because
  `classify_gated_feature` never returns `GatedStandalone`.
- A pending version on the gated default plus `--standalone` bumps again (the inherited-
  pending rule, with its notice) vs. refusing and pointing at `bump finish`: chose
  consistency with the feature-branch row Scott ruled on. The notice still tells the
  operator how to ship the inherited version first.
- Tests for the tag_ladder split live in `src/release/tests/finish.rs`, not in `main.rs`'s
  test module, to keep `main.rs` from growing.

### Open questions
- `src/main.rs` is 2396 lines (2394 before this phase), over the 1500-line cap in
  rules/rust.md. That predates this doc. Split it before Phase 6 touches `dispatch_*`
  and the after-help, or leave it?

## Phase 4 addendum: standalone on a pending version refuses

- Ruling (team lead, from the doc): both standalone rows are scoped to a TAGGED default
  (design doc lines 82-84). An untagged version on a gated default is the `bump finish`
  state (finish table row 1), and the "bump again" ruling covers a feature branch carrying
  work, not a bump-only standalone branch. This supersedes Phase 4's tradeoff "a pending
  version on the gated default plus `--standalone` bumps again".
- `classify_gated_standalone` (`src/release/standalone.rs`) now returns the new
  `ReleaseState::GatedStandalonePending { pending, default }` on `PendingCheck::Pending`. It
  refuses before any checkout, naming the pending `vX.Y.Z` and `Run: bump finish`.
- Ungated side unchanged, and now pinned by a test: the pending version is classified before
  the standalone split, so `--standalone` on an ungated default with a pending version takes
  the `UngatedPending` row and only prints the order.
- Test: `standalone_on_a_pending_version_refuses_naming_bump_finish`. Gated: no branch cut,
  no commit, no push, no PR probe, no tag. Ungated twin: resumes `v0.1.6`, no version commit,
  only the tag push.

## Phase 5: finish from any worktree

### Design decisions
- `finish_dir` (`src/release/finish.rs`) is read-only and returns `FinishDir::{Own,
  Sibling(path), CheckoutHere}`: current branch == default -> own checkout; else
  `git::worktree_for_branch` -> the sibling; else check out here. The checkout for
  `CheckoutHere` happens later, in `reach_merged_tip`, after classification, so nothing
  moves before a refusal.
- Order in `finish`: git-repo check -> resolve -> tracked changes on the current worktree,
  then on the resolved one (named by path) -> generic check on the resolved worktree ->
  dry run -> `reach_merged_tip` -> `config::load(work)` -> `tag_ladder(work)` -> gate, tag,
  push, install, all on `work`.
- `reach_merged_tip`: `fetch_branch`, then the new `git::compare_branch_to_remote` (the
  LOCAL `refs/heads/<default>` vs `origin/<default>`, whether or not it is checked out
  here). Behind -> `pull --ff-only` after the checkout; Equal -> nothing; Ahead -> refuse
  with the literal rescue (`git branch stranded-<sha8>` + `git reset --hard origin/<default>`
  after `cd <worktree>`, or `git branch -f` when the default is checked out nowhere, same
  shape as the release verb's `GatedStranded`); Diverged -> refuse naming `cd <worktree> &&
  git pull --rebase origin <default>`, then re-run. No local default at all -> the checkout
  creates it from origin (DWIM), after which the ladder sees Equal.
- `compare_head_to_remote` now delegates to a shared `compare_rev_to_remote`, so HEAD and
  branch comparisons use one ancestry rule. `git::rev_parse` became `pub` for the rescue
  branch name. `worktree_for_branch` lost its `#[allow(dead_code)]`.
- Already released (remote tag at the merged tip) runs `run_install` in the resolved
  worktree; `--no-install` (`InstallChoice::Skip`) skips it.
- Dry run resolves the worktree (read-only) and prints where it would finish, plus the
  classify-then-pull steps. It still fetches nothing.
- Test `finish_remote_tag_is_clean_noop_across_two_runs` was renamed and inverted to
  `finish_already_released_still_installs`. It pinned "no install on already released".
- `RecordingInstaller` records the dir too, so the worktree test asserts install ran in
  the main worktree.
- Tests (`src/release/tests/finish.rs`): the four named criteria, plus
  `finish_ahead_default_refuses_before_pull`,
  `finish_refuses_tracked_change_in_the_resolved_worktree`,
  `finish_creates_a_missing_default_branch_from_origin`; each worktree test compares a
  snapshot (branch, HEAD, `status --porcelain` with an untracked scratch file, Cargo.toml)
  of the feature worktree before and after. `git::tests::compare_branch_to_remote_reads_the_branch_not_head`
  covers the helper.
- Break-the-code checks, run by hand and reverted: letting Diverged fall through to the
  pull fails `finish_diverged_default_refuses_before_pull` (git's ff-only error instead of
  the rebase line); forcing `CheckoutHere` over a found sibling fails
  `finish_from_feature_worktree_finishes_in_the_default_worktree` ("'main' is already used
  by worktree").

### Deviations
- `finish_dir` does not check out; it only decides. The doc lists "checkout here" as part
  of resolution, but the same doc requires classification before anything moves. Same
  effect, correct seam.
- The Ahead refusal wording is not in the doc (it says only "refuse before any pull");
  it follows the release verb's stranded rescue so the refusal names exact commands.

### Tradeoffs
- Classifying the local branch ref vs checking out first and classifying HEAD: the ref
  comparison is what lets the `CheckoutHere` case refuse without switching branches.
- Treating a missing local default as "nothing to classify" vs refusing: a clone with only
  a feature branch is a normal state, and the checkout creates the branch at origin's tip,
  which cannot carry local-only commits.

### Open questions
- The Acceptance Criterion `git show HEAD:src/release/tests.rs | grep -c 'fn red_ci_leaves_no_tag\|fn gated_standalone_cuts\|fn finish_from_feature_worktree'`
  predates the module split: those tests now live in `src/release/tests/{gate,standalone,finish}.rs`,
  so that exact command prints `0`. Rewrite the criterion to grep `src/release/tests/`?

## Phase 6: CLI surface and docs

### Design decisions
- `ReleaseArgs` (`src/cli.rs`) gained `--standalone <WORDS>` (`Option<String>`),
  `--no-ci-gate` (`bool`), `--ci-timeout <SECS>` (`u64`, `default_value_t =
  default_ci_timeout_secs()`); `FinishArgs` gained the same two CI flags. `main.rs`'s
  `dispatch_release`/`dispatch_finish` no longer build `ReleaseOpts`/`FinishOpts` inline --
  the mapping moved into two pure functions, `release_opts_from_args`/
  `finish_opts_from_args`, so the CLI -> opts conversion is unit-testable without touching
  git: `bump_type` stays `Option<BumpType>` (`None` on no level flag, unchanged from Phase
  3), `standalone: args.standalone.clone()`, `ci_gate: !args.no_ci_gate`, `ci_timeout:
  Duration::from_secs(args.ci_timeout)`.
- `--ci-timeout`'s default is NOT a duplicated literal: `cli.rs` imports
  `release::DEFAULT_CI_TIMEOUT` and renders it through `default_ci_timeout_secs() ->
  u64` (clap's `default_value_t` needs the plain integer it prints in `--help`, not a
  `Duration`). One source of truth; the CLI default and the verb's own default can't drift.
- `bump release --help` / `bump finish --help` after-help rewritten (`get_release_help`/
  `get_finish_help`, `src/cli.rs`) to the two flows, the condensed state table (API Design
  section, Phases 3-5's actual rows), RESUME, Scott's order flag, the CI wait, and the
  persona-token resolution order. Deliberately does NOT repeat the literal flag names
  (`--standalone`, `--no-ci-gate`, `--ci-timeout`) anywhere in the after-help prose --
  refers to them as "the order flag above", "the timeout flag above", "the skip flag
  above" instead. That is what makes the acceptance criterion (`bump release --help |
  grep -cE 'standalone|no-ci-gate|ci-timeout'` == `3`) come out exactly `3`: clap's own
  Options block is the only place the three names appear. Verified live (see Deviations):
  `3`.
- Default install (`resolve_install`, `src/release.rs`): the `Auto` arm now calls the new
  `lang::cargo::has_package_table(dir)` instead of `cargo_toml_exists(dir)`. A virtual
  workspace root (`[workspace]`, no `[package]`, `tatari-tv/marquee`'s shape) no longer
  gets the bare `cargo install --path .` default, which fails on it. `install_skip_reason`
  (new, private to `release.rs`) prints the specific "skipped (virtual workspace root;
  pass --install or set install: in bump.yml)" line only for the `Auto` choice on a
  tracked `Cargo.toml` with no `[package]` table; every other skip (`--no-install`, or no
  Cargo manifest at all) still prints the plain "skipped". `run_install` and `echo_install`
  (the `-n` echo) both gained `dir`/`choice` parameters to call it; all four call sites
  (two in `release.rs`, one in `finish.rs`, one shared) updated.
- `has_package_table` (`src/lang/cargo.rs`): parses `Cargo.toml` and checks `doc.get
  ("package").is_some()`. Deliberately simpler than reusing the existing private
  `is_workspace_only` (which also asserts `workspace.is_some()`) -- the install decision
  only cares whether a `[package]` table exists to install, not whether a `[workspace]`
  table is also present alongside it (a workspace member's own `Cargo.toml` has both keys
  absent for `[workspace]` typically, but the positive check is what the call site needs).
- README (`README.md`, "The release verbs" section) rewritten to the design doc's API
  Design tables: the two flows, the condensed release/finish state tables, the standalone
  order paragraph, a CI gate paragraph, the install precedence (now naming the `[package]`
  table requirement and the virtual-workspace-root skip message), and the persona-token
  resolution order.

### Deviations
- None from the doc's Phase 6 bullets. `otto ci` green: `cargo run -q -- release --help |
  grep -cE 'standalone|no-ci-gate|ci-timeout'` prints `3` (verified live, matching the
  design doc's Acceptance Criteria list), and `cargo run -q -- finish --help | grep -cE
  'standalone|no-ci-gate|ci-timeout'` prints `2` (finish has no `--standalone`, so its
  count is the two CI flags, not the doc's 3-flag release criterion).

### Tradeoffs
- Extracting `release_opts_from_args`/`finish_opts_from_args` as free functions in
  `main.rs` vs. inlining the flag mapping into `dispatch_release`/`dispatch_finish` as
  before: chose the extraction so the "clap parse tests assert ... reach `ReleaseOpts` as
  `Some(words)`, `ci_gate == false`, the given `Duration`, and `bump_type == None`"
  criterion can be a pure unit test (parse with `Cli::try_parse_from`, convert, assert
  fields) instead of needing a live git fixture just to prove a flag reached a struct
  field. `src/main.rs` gained two short functions and no new fixture-dependent tests for
  that criterion; the fixture-dependent proof (the flag reaching EXECUTION, not just the
  opts struct) is the `tests/release_cli.rs::release_dry_run_no_ci_gate_prints_ci_gate_
  skipped` end-to-end test instead.
- The end-to-end "prints `CI gate: SKIPPED`" criterion needed a real (if minimal) ungated
  git fixture with a bare local `origin` and `BUMP_GATES_PROBE=ungated`, run with `-n` so
  no real push/tag/install happens: `tests/release_cli.rs` (new), following
  `tests/skip_member.rs`'s pattern (`CARGO_BIN_EXE_bump`, real stdout, no `gh`/network).
  The origin bare repo lives in its OWN `TempDir`, not a subdirectory of the work checkout
  -- an earlier version nested it under the work dir and `bump release` refused on the
  resulting dirty tree (the bare repo showed up as an untracked path).
- `install_skip_reason` as a separate function from `resolve_install` vs. folding the
  reason string into `resolve_install`'s return type (e.g. `Result<Option<String>,
  SkipReason>`): kept `resolve_install`'s signature (`Option<String>`) unchanged since
  Phase 2 callers and tests already depend on it; the reason is cheap to recompute from
  the same two inputs (`dir`, `choice`) at the print site.

### Open questions
- None.

## Finalization: acceptance-criteria amendments

Team-lead review of Phase 6 (before acceptance) flagged that the after-help's phrasing
had been contorted to fit a defective acceptance criterion rather than the other way
round. This entry supersedes the "Deviations" line above that cited a passing
`grep -cE 'standalone|no-ci-gate|ci-timeout'` count of `3`/`2` -- that line-count form is
now understood to have been the wrong criterion, not evidence the code was right; it is
left in place above (append-only) rather than edited.

### Design decisions
- `get_release_help`/`get_finish_help` (`src/cli.rs`) rewritten AGAIN to name
  `--standalone "<words>"`, `--no-ci-gate`, and `--ci-timeout` literally wherever the
  prior commit had paraphrased them ("the order flag above", "the timeout flag above",
  "the skip flag above", "SCOTT'S ORDER (the flag above with WORDS)"). An agent (or
  Scott) reading `--help` needs to see the flag spelled out, not decoded from a
  cross-reference.
- `src/cli.rs`'s unit test renamed from `test_release_after_help_does_not_repeat_the_
  three_flag_names` (asserted zero repetitions) to
  `test_release_after_help_names_all_three_flags_literally` (asserts each of the three
  substrings is present at least once). `tests/release_cli.rs`'s
  `release_help_names_the_three_flags_exactly_three_times` (a line count against the
  compiled binary) renamed to `release_help_names_all_three_flags_at_least_once`
  (dedups a found-set and asserts its size is `3` -- the same shape as the doc's amended
  criterion, so the test and the criterion cannot drift apart the way the line-count
  form did).

### Deviations
- The doc's original first Acceptance Criterion (`grep -cE
  'standalone|no-ci-gate|ci-timeout'` prints `3`) is a LINE-COUNT of every line
  mentioning any of the three substrings. It is independent of whether the code is
  right: it is satisfiable only by an after-help that mentions each flag EXACTLY once in
  the whole document, which is a constraint on prose density, not on flag coverage. The
  design doc's own Phase 6 bullet requires the after-help to "cover ... `--standalone`
  ... the CI gate" in prose -- doing that honestly (naming the flags where they are
  discussed) necessarily produces MORE than 3 matching lines once the Options block's own
  3 lines are added to the prose's mentions. Amended to `grep -oE --
  '--(standalone|no-ci-gate|ci-timeout)' | sort -u | wc -l` prints `3`: presence of each
  of the three flags at least once, order- and count-independent. Observed on this
  branch: `3` (`--ci-timeout`, `--no-ci-gate`, `--standalone`, alphabetical from `sort
  -u`).
- The doc's test-name Acceptance Criterion (`git show HEAD:src/release/tests.rs | grep
  -c 'fn red_ci_leaves_no_tag\|fn gated_standalone_cuts\|fn finish_from_feature_
  worktree'`) greps a single file, `src/release/tests.rs`, that no longer contains any of
  the three tests: the Phase 3 addendum's module split (commit `46aed56`, ahead of the
  1500-line-per-file cap at `rules/rust.md:113`) moved test FUNCTIONS out of
  `tests.rs` into `src/release/tests/{gate,standalone,finish}.rs` while `tests.rs` kept
  only the shared doubles/harness/fixtures. The criterion therefore prints `0` on every
  commit since `46aed56`, including every one where the three tests demonstrably exist
  and pass -- it stopped measuring anything the moment the split landed, independent of
  Phase 6. Amended to `git -C ~/repos/scottidler/bump grep -c -E 'fn
  (red_ci_leaves_no_tag|gated_standalone_cuts|finish_from_feature_worktree)' HEAD --
  src/release/tests/ | awk -F: '{s+=$NF} END {print s}'`: sums per-file match counts
  across the whole post-split test directory, so it survives future splits inside that
  directory too. Observed on branch `one-release-command` (`ed8640c`): `3` --
  `src/release/tests/gate.rs:9:fn red_ci_leaves_no_tag_and_green_rerun_resumes_same_
  version`, `src/release/tests/standalone.rs:57:fn gated_standalone_cuts_tracking_
  branch_bumps_and_quotes_scott`, `src/release/tests/finish.rs:310:fn finish_from_
  feature_worktree_finishes_in_the_default_worktree`.

### Tradeoffs
- Renaming the two after-help tests in place (`src/cli.rs`,
  `tests/release_cli.rs`) vs. adding new tests alongside the old ones: renamed, because
  the old assertions (zero repetition; exact line count of 3) actively contradict the
  corrected requirement (the flags must be named) -- keeping both would leave one test
  permanently red or one test asserting the wrong thing forever.

### Open questions
- None.

## Implementation audit round 1: fold

Source: review-panel round 1 synthesis (must-fix 1-2, cheap-win 3-6, doc-only 7-8). Each
must-fix and cheap-win was reproduced by a failing test before the fix.

### Design decisions
- Must-fix 1, ungated generic repos: `gate_tag_and_push` (`src/release/tag.rs`) skips
  ONLY the manifest-version sub-check when `detect_project_type == Generic`; sha ==
  origin tip equality is kept at both re-verifies. A tip that moved during the CI wait
  refuses for generic (no manifest can prove the new tip is the same release) instead of
  following it. Tests: `ungated_generic_release_tags_and_pushes`,
  `ungated_generic_red_ci_rerun_resumes` (`src/release/tests/ungated.rs`).
- Must-fix 1, generic resume: `classify_generic_resume` (`src/release.rs`) makes an
  ungated generic tip at HEAD == origin that no remote tag names the RESUME row: the
  latest local tag when it sits at HEAD unpushed (the local-tag resume row), else the tag
  the level computes from the latest tag. A computed tag that origin already carries but
  the local repo lacks refuses naming `git fetch --tags origin`. Generic has no manifest,
  so "the version is untagged" is read off the tip, the only version source it has.
- Must-fix 2, package version only: new module `src/release/version_diff.rs` compares
  each root manifest at the merge base with `origin/<default>` and at HEAD, split into
  the PACKAGE version line (Cargo `[package]`/`[workspace.package]`, pyproject
  `[project]`/`[tool.poetry]`, package.json top-level `"version"` by brace depth, the
  whole `VERSION` file) and everything else. `version_line_changed` (the branch's own
  bump) and `is_bump_only_branch` (`src/release/standalone.rs`) both read that split.
  `git::is_version_diff_line`, `git::version_line_changed`, `git::changed_lines` and
  their tests were removed; `version_line_changed_tells_own_bump_from_work_only` moved to
  `version_diff::tests::version_line_changed_tells_own_bump_from_work_and_dependency_bumps`
  with the dependency-table case added. Tests:
  `dependency_table_version_change_with_work_bumps_fresh`,
  `dependency_table_only_change_is_not_bump_only` (`src/release/tests/standalone.rs`),
  plus the split unit tests in `version_diff.rs`.
- Cheap-win 3: `git::push_tag` passes `--no-follow-tags` (the dry-run echo in
  `tag::echo_tag_steps` says so too). Test:
  `git::tests::push_tag_pushes_only_the_named_tag_under_follow_tags`.
- Cheap-win 4: `github::check_runs_from_json` errors when `total_count` is missing or not
  a non-negative integer. Test:
  `check_runs_from_json_missing_or_non_integer_total_count_is_a_loud_error`.
- Cheap-win 5: the `Pr` port gained `comment_pr` (`gh pr comment <branch> --body`,
  `github::comment_pr`); `execute_gated` comments `Standalone release ordered by Scott:
  "<words>"` on an already-open PR and prints it; the dry run echoes the comment step.
  The body line and the comment share `pr::standalone_order_line`. Test:
  `standalone_order_is_commented_on_an_already_open_pr` (`src/release/tests/pr.rs`).
- Cheap-win 6: `node::read_version` names the full path again ("Failed to parse <path>
  as JSON", "... (found 1) in <path>"), the v0.3.3 text; the blob reader still says
  `package.json`. Shared parse: `node::parse_top_level_version`. Test:
  `read_version_errors_name_the_file_path`.

### Deviations
- Doc correction (item 7): the tag-race sentence in "Tag placement and the two
  re-verifies" said the local-tag resume row re-runs the gate on the tip. It now says what
  the code does: fails safe, and the refusal names `git tag -d <tag>` for when origin no
  longer carries the tagged sha.
- Doc correction (item 8): `is_head_pushed` checks every remote-tracking ref (`git branch
  -r --contains HEAD`), not only `origin/*`; the Architecture bullet for `src/git.rs` and
  the Phase 1 bullet now say so.
- Doc correction (must-fix 1): a new ungated generic RESUME row in the release table.
- Doc correction (must-fix 2): "Bump-only branch" and "Whose pending version it is" now
  scope the version line to the PACKAGE version, noting the hook's regex would count a
  dependency-table line.
- Doc correction (cheap-win 5): the two `--standalone` bullets say the order goes onto an
  already-open PR as a comment.
- Unrelated test flake fixed: `tests::amend_path_honors_explicit_message`
  (`src/main.rs`) creates a tag, and tagging consults `BUMP_GATES_PROBE`, but it did not
  hold `ENV_LOCK`; it failed once with the gated refusal while a gated test held the
  probe. It now takes the lock. The other unlocked `process_directory` tests all run
  `--no-tag` and never read the probe.

### Tradeoffs
- Generic resume reads "untagged tip" as pending vs refusing ungated generic before any
  push: resume keeps the v0.3.3 behavior (ungated generic releases) and recovers from a
  red CI. Side effect: an ungated generic repo whose pushed tip was never tagged now
  releases on a bare `bump release` instead of refusing "nothing to release".
- Package-version split by line scanning (TOML table headers, JSON brace depth) vs
  parsing both sides with `toml_edit`/`serde_json`: scanning keeps "any other byte
  changed" exact (formatting changes count as work, like the hook), where a parsed
  compare would hide them. Cost: a `[` at the start of a line inside a multi-line TOML
  array would be read as a table header.
- Cheap-win 5 comment vs print only: a comment keeps the order on the PR, the audit trail
  the doc promises, where a print survives only in the transcript. Cost: each re-run with
  `--standalone` against the same open PR adds another comment.

### Open questions
- Is the generic side effect above acceptable (an untagged pushed tip in an ungated
  generic repo releases on a bare `bump release`), or should that row demand the tip be a
  prior run's push (no marker exists today)?
- Hook parity (Phase 8): `git-release-guard.sh` Gate D and `is_bump_only_ref` still use
  the line regex that counts `[dependencies.<name>] version`.
