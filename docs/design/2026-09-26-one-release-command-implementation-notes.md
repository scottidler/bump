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
