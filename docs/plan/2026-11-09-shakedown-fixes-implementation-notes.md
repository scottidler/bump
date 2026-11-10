## Phase 1: Kill the flaky verify test
### Design decisions
- Write the fake binary via a child `install -m 755` from a staged `tool.src` (src/release/tests/install.rs:fake_install_root): the exec'd file is never opened for write by the test process, so a parallel fork cannot inherit a write handle on it.
- The staged source (`tool.src`) is still written in-process; it is never exec'd, so a leaked handle on it is harmless.
### Deviations
- None.
### Tradeoffs
- Stage-then-`install` vs. spawning `sh -c 'cat > ...'` or a retry on ETXTBSY: `install` is the doc's named mechanism and removes the race rather than papering over it.
### Open questions
- None.

## Phase 2: Land leave-pr-to-pr-create (bug 3)
### Design decisions
- Cherry-picked 3208457 onto main rather than rebasing the branch, so the `leave-pr-to-pr-create` ref stays untouched; 174d640 is already on main and was skipped.
- Resolved the `tests/release_cli.rs` conflict by keeping both appended tests (`release_dry_run_on_a_service_prints_the_clap_skip_and_no_verify` and `gated_release_never_spawns_gh_pr_create`), each with its own closing brace.
### Deviations
- None. The doc's stale-prose bullet is conditional ("if 3208457 left any"); `src/cli.rs` release/finish help and the `src/main.rs` gates report already say bump never opens a PR, so no prose edits were needed.
### Tradeoffs
- Cherry-pick plus amend vs. a rebase: the orchestrator asked for the branch ref to stay put, and the result is one Phase 2 commit on main.
### Open questions
- None.

## Phase 3: Land release-verbs-take-dir
### Design decisions
- Cherry-picked eba0468 onto main rather than rebasing, so `origin/release-verbs-take-dir` and every other branch ref stay untouched; one Phase 3 commit on main.
- Resolved the `README.md` conflict by keeping HEAD's "the open-PR probe" wording (Phase 2) and adding eba0468's "or pass `[DIR]` to name it (default: the cwd)" clause.
- Subject reworded to "Release and finish take an optional directory" (no `feat(cli):` prefix).
### Deviations
- None. No code changes beyond the cherry-picked diff; `src/cli.rs` and `src/main.rs` applied cleanly.
### Tradeoffs
- Cherry-pick plus amend vs. a rebase: the orchestrator asked for no branch ref to move, and this yields one commit.
### Open questions
- Deleting the merged `release-verbs-take-dir` branch (local and origin) is in the doc's Rollout Plan but was not done: this run was told not to touch branch refs. The parent should do it after the phases land.

## Phase 4: Repo-root resolver (bug 1)
### Design decisions
- `git::repo_root` (`src/git.rs`) runs `git rev-parse --show-toplevel --show-prefix` and drops the prefix's trailing `/`. The empty prefix decides "already at the root", per the Architecture section.
- One resolver, `resolve_run_dir` (`src/main.rs`), returns data (`RunDir { path, key, notice }`). The plain-bump loop and `resolve_verb_dir` print the notice. Every entry point goes through one of those two callers: plain bump, `--gates`, `--tag-only`, `release`, `finish`, and a verb's `[DIR]`.
- Outside a work tree (no repo, a bare repo, inside `.git`, a missing dir), `resolve_run_dir` passes the argument through unchanged, so each entry point's existing "not a git repository" error fires word for word. The doc requires this ("a non-repo dir keeps today's error").
- At the root (empty prefix), bump runs on the argument as given, not on git's resolved top-level. A symlinked spelling of a root keeps that spelling in every message and `[name] Done` line.
- Below the root, `spelled_root` strips the prefix's components off the argument and uses that as the root when it canonicalizes to git's top-level. Otherwise it uses git's top-level, for example when the argument is itself a symlink into the tree or carries `..`. This keeps a symlinked TMPDIR's spelling in the notice.
- Dedup key: git's canonical top-level. `bump <root> <root>/src` and `bump <link-to-root> <root>` each run once. A non-repo argument keys on itself.
- The plain-bump loop resolves every argument first, then runs them. Notices go out before any `[name]` header. A refusal is held and reported in its slot as a per-directory error, so the batch still exits 1 and its siblings still run. `runs.len()` (after dedup) drives the header and the batch summary.
- The generic-root refusal and the "(ignoring ...)" suffix both use `nearest_nested_manifest`. It walks from `root/prefix` up to, but not including, the root, and returns the first `lang::detect` hit as a path relative to the root. The refusal names that relative path (`plugin/Cargo.toml`). The suffix splits it into file and directory (`(ignoring Cargo.toml in tools)`).
### Deviations
- The Phase 4 binary tests live in a new `tests/subdir.rs`, not in `tests/release_cli.rs` (Testing Strategy). Three of them cover entry points that are not `release` (plain bump, `--gates`, `--tag-only`), and the fixture helpers differ. Same coverage, clearer home.
- The doc says "<dir> or any directory between it and the root has one" for the refusal but only "<dir> has its own too" for the suffix. Both now use the same walk, including intermediate directories, so a manifest between the argument and the root is never ignored silently.
- The refusal reaches the operator through the normal error path, so it prints as `Error: bump: <dir> is ...`. The doc's text follows `Error: ` word for word.
### Tradeoffs
- Dropping a duplicate argument without a notice (it goes to the `info!` log) vs. printing the notice for each duplicate. The doc says "once per resolved directory", and the kept run already prints the notice when there is one.
- Passing a non-repo through vs. failing in the resolver. Passing through keeps each entry point's existing error text and the existing git-missing path, which the doc defers as unchanged.
- Rewrote the three eba0468 `resolve_verb_dir` tests against real repos (round 1 Q3). `test_finish_verb_honors_dir_positional_and_walks_up_from_a_subdir` now also proves the walk-up, and `test_verb_dir_defaults_to_cwd_and_absolutizes_relative` resolves the relative `src` against this repo's root, which is the cwd under `cargo test`.
### Open questions
- None.

## Phase 5: --skip-member fails closed (bug 2)
### Design decisions
- `conflicts_with_all = ["tag_only", "gates"]` on `--skip-member` (`src/cli.rs`, `Cli::skip_member`): neither mode reads the skip list, so a swallowed directory could not fail closed there.
- Hint appended to the unknown-member error (`src/lang.rs`, `validate_project`): "If a value is a directory, put directories before --skip-member." No parse-time directory check, per the Resolved Decisions (a member name can be a sibling directory).
- `test_cli_skip_member_swallows_trailing_positional` kept as is (the parse is unchanged); only its comment now says it fails closed in every mode the conflicts leave.
- New tests: `skip_member_conflicts_with_tag_only_and_gates` and `skip_member_accepts_sibling_directory_member_name` (`src/cli.rs`); the hint is asserted in `validate_stale_skip_member_aborts` (`src/main.rs`).
### Deviations
- No `tests/release_cli.rs` binary test: the parse-level clap test pins the conflict directly, and the criterion was run against the real binary on the t4/t5 fixtures. Same coverage, correct seam.
### Tradeoffs
- Asserting the hint in the existing stale-skip test vs. a new test: same call, same error, one fewer fixture.
### Open questions
- None.

## Phase 6: Plain-bump dry-run tells the truth (bug 4, C1, C4)
### Design decisions
- `-n` runs the real run's clean-tree refusal (`!has_changes && head_has_tag && !--force`) before printing anything about the plan (`src/main.rs`, `process_directory`). The message is one const, `HEAD_ALREADY_TAGGED`, used by both paths so they cannot drift. The refusal fires after the `bump: X -> Y` line, as in the real run.
- The dry-run wording is a pure function, `dry_run_lines(&DryRunPlan)`, so the shapes are unit-tested without a repo. With `--force` on a tagged, pushed HEAD it plans a new commit; unpushed, an amend (the same `is_head_pushed` / `never_amend` choice as the real run).
- One commit-and-tag wording: `Would commit and tag: <tag>` for both uncommitted changes and a pushed clean HEAD. The doc names the problem ("two wordings") but not the survivor; I kept the shorter one, which matches the real run's "Committed and tagged". `Would amend previous commit and tag` stays, since it is a different action.
- Added `Would tag: <tag>` for the dry-run of a repo where nothing would be staged (generic, clean), mirroring the real run's new `Tagged <tag>` line. Wording is mine.
- Manifest listing: `manifest_file_names` uses each manifest's `version_files()` (manifest plus any existing lockfile), relative to the directory, comma-joined on one `Would update:` line (`Would update: Cargo.toml, Cargo.lock`). A lockfile that does not exist yet is not listed; the sync only touches one that exists.
- C1 "from": `bump: v0.6.0 -> 0.6.1`, the raw latest-tag string, falling back to `unknown` only when there is no tag and no manifest version. The doc's criterion shows the `v` prefix, so I did not strip it.
- C1 "Tagged vX": `tagged_line` replaces "Committed and tagged" / "Amended commit and tagged" with `Tagged <tag>` whenever no file was staged, in all three real-run branches.
### Deviations
- Binary tests live in a new `tests/dryrun.rs`, not `tests/release_cli.rs`, for the same reason as Phase 4's `tests/subdir.rs` (plain bump, own fixture helpers).
- The doc says "Dry-run lists each manifest with its lockfile" as separate items; I print them on one line rather than one line per file.
### Tradeoffs
- Reusing a shared const for the refusal vs. extracting a check function: the check is one condition and the dry-run needs `bail!` at the same point, so the message const is the smallest thing that cannot drift.
- Showing `Would tag:` for the nothing-to-stage case vs. leaving it under "Would commit and tag": the latter is the same false claim C1 fixes in the real run.
### Open questions
- None.

## Phase 7: Release and finish refusals (bug 5, bug 6, C6)
### Design decisions
- `ReleaseState::DirtyTree { files }` holds raw `git status --porcelain` lines (`?? scratch.txt`, ` M Cargo.toml`) - `src/release.rs:classify`, new `git::porcelain_lines` in `src/git.rs` - the doc says "porcelain lines"; the existing `dirty_files` strips the status columns, so a new fn returns them whole.
- Dirty refusal wording (doc gave only the advice list): "the working tree is dirty; bump release only performs the mechanical release on a CLEAN tree.", the file lines, then "Commit them, `git stash -u`, remove them, or gitignore them, then bump release" - `src/release.rs:execute` - "Commit" capitalized as a sentence start.
- C6 wording: "bump finish does not support generic repos: it cannot derive a version without a manifest." - `src/release/finish.rs:finish`.
- Empty-install check is one shared `check_install_not_empty(choice, config)` - `src/release.rs` - precedence by construction: `Skip` returns Ok, `Command` checks only the flag value, `Auto` checks only `config.install`. The value is trimmed, so whitespace-only counts as empty.
- `release()` runs the check right after `config::load`, before `classify`, so an empty install refuses regardless of repo state.
- `finish()` checks `config::load(work)` (the worktree it picked) before any mutation and in the dry run, and checks again after `reach_merged_tip` with the merged tip's config, since that is the config the install actually uses.
### Deviations
- `finish_gated_generic_repo_refuses` was updated to assert the new message and that it no longer says "Gated" (the old wording pinned by name, inverted).
- The release `GatedGeneric` refusal keeps "Gated generic repos are unsupported": that path did check the gate, so C6 (finish-only) does not apply.
### Tradeoffs
- A second post-merge check in `finish` vs one early check: a tracked `bump.yml` could change in the merge; the extra check costs one comparison.
### Open questions
- None.

## Phase 8: True verify verdicts (bug 7)
### Design decisions
- `split_command_words` returns a `CommandWords { words, shell_syntax }` struct - `src/lang/cargo.rs:split_command_words` - the doc says it "also reports" shell syntax; a named field reads better at the one call site than a tuple. Syntax characters are reported, not interpreted: `a;b` stays one word and the caller skips on the flag.
- The recognizer consumes leading `NAME=value` words (`assignment_name`, `[A-Za-z_][A-Za-z0-9_]*`), an optional bare `env`, more `NAME=value` words, then `cargo [+toolchain] install` - `src/lang/cargo.rs:cargo_install_target`. `env -i ...` or any other `env` flag is outside the shape and skips as "not cargo install".
- The arg loop no longer returns `NotThisRepo` on the first positional: it records it and keeps scanning, so a later `--root` still yields `CustomRoot` (reason 3 before reason 4, the doc's order).
- Containment runs only when `--path` is given: `dir.join(p)` and `dir` both canonicalized; either failing (the target does not exist) falls through to `Dir`, which the binary-name check then skips, as the doc says. `Dir` keeps the as-written path (components-collected), so the existing `/repo` table tests and the printed manifest path are unchanged.
- `InstallTarget` variants are declared in check order, with `ShellSyntax` first.
### Deviations
- `$` and backtick inside double quotes count as shell syntax, and `\$` / `` \` `` inside double quotes are now escapes (literal `$` / backtick). The doc says "unquoted, unescaped"; `sh` expands `$X` and backticks inside double quotes, so `--path "$X"` would otherwise read a literal `$X` path and skip for the wrong reason. Single quotes stay fully literal.
- The outside-repo reason prints the canonical target (`install target /.../other is outside this repo`), since the doc leaves `<p>` unspecified and the canonical path is the one that was checked (it names the real target of an in-repo symlink).
- Binary tests live in a new `tests/verify.rs`, not `tests/release_cli.rs` (own `t4` + sibling fixture, same reason as Phase 4's `tests/subdir.rs`).
### Tradeoffs
- Flagging `$` inside double quotes vs. the doc's literal "unquoted": the doc's goal is a true reason, and the extra case only turns a misleading binary-name skip into the shell-syntax skip; nothing that verified before stops verifying.
- Printing the canonical path vs. the `--path` value as written: the written `../other` is shorter, but for a symlink it would hide where the install actually reads from.
### Open questions
- `~` (tilde expansion), glob characters and a `#` comment are not in the doc's shell-syntax set. `--path ~/x` joins to `<repo>/~/x`, which does not exist, so it skips on "no binary name" (a skip, never a wrong verify, but not the true reason). Add them to the set?
- Phase 12's README bullet says "six verify skip reasons"; the Data Model now lists seven. Phase 12 should document seven.

## Phase 9: Install and verify messages (C3, S2, S3, S4)
### Design decisions
- The re-run line is now `verify_rerun(root)`, not a const, and `run_install` appends it to any install or verify error via `with_rerun` - `src/release.rs` - one place owns the closing line and the repo root (the `Installer::verify` dir is the install target, which can be a member, so only `run_install` knows the root). `ShellInstaller` errors no longer carry it.
- The root in the re-run line is canonicalized (falls back to the path as given) - `verify_rerun` - the caller may have passed a relative dir or run from anywhere, and the line must be runnable as is.
- Install failure text is `install command failed: <cmd> (<status>)`, which prints `exit status: 7` - `ShellInstaller::install` - wording chosen; the doc gave none beyond "the exit status".
- C3 reads `exited with exit status: 3` - `ShellInstaller::verify` - the doc's `exited with {status}` applied literally; `ExitStatus` Display supplies the "exit status:" prefix.
- S4: `which_on_path` walks PATH for the first executable file and `installed_bin_path` returns it absolute; falls back to the bare name when nothing resolves - `src/release.rs` - the fact line `verify ran <path> (from PATH)` and the executed path now agree.
- Old test `install_skip_reason_is_plain_with_no_cargo_manifest` inverted (renamed `..._names_the_missing_cargo_manifest_not_a_bare_skip`) because it pinned the bare "skipped" S2 removes.
### Deviations
- None.
### Tradeoffs
- Appending the re-run line in `run_install` vs passing the root into the `Installer` trait methods - keeps the trait signature unchanged and the doubles untouched.
### Open questions
- None.

## Phase 10: --gates (C2, S5)
### Design decisions
- UNKNOWN is an `Err` from `report_gates` after the report prints - `src/main.rs:report_gates` - the existing per-directory error path prints `Error: gate verdict UNKNOWN: <reason>` and the run exits 1, so a multi-repo run still reports every repo before failing.
- Under `--gates` the batch footer prints only `Completed: N succeeded, M failed` on failure and nothing on success - `src/main.rs` main loop - `--gates` changes nothing, so "push your changes" is never true.
- `conflicts_with = "no_verify"` on `--gates`, and the `--gates` help says it exits 1 on UNKNOWN - `src/cli.rs`.
- `Branch:` -> `Default:` in the report and the README example.
- Binary tests in a new `tests/gates.rs` (UNKNOWN exit 1 with report, known verdicts exit 0, no "All done!" in multi-repo, `--gates --no-verify` clap error exit 2); unit `gates_conflicts_with_no_verify` in `src/cli.rs`. `report_gates_runs_for_each_verdict` renamed `report_gates_errors_only_on_unknown` because it pinned UNKNOWN succeeding.
- `--no-tag` still skips the gate probe; `no_tag_skips_gate_probe_even_when_gated` untouched and passing.
### Deviations
- None.
### Tradeoffs
- Leaving `Repo:` and `Gates:` at their old padding, so `Default: ` is one column wider than its siblings, vs re-padding every label - the re-pad touches several tests and the README for a cosmetic gain.
### Open questions
- None.

## Phase 10 (superseding note): report column alignment
### Design decisions
- `Repo:` and `Gates:` re-padded so `Repo:`, `Default:` and `Gates:` values share one column - `src/main.rs:report_gates`, README example, `tests/gates.rs`, `tests/subdir.rs` - a cosmetic phase must not add a misalignment.
### Deviations
- None.
### Tradeoffs
- Supersedes the earlier Phase 10 Tradeoffs entry (left unaligned to avoid touching tests and README); the re-pad was done after all.
### Open questions
- None.

## Phase 11: State-aware finish -n (S1)
### Design decisions
- `classify_default` split out of `reach_merged_tip` - `src/release/finish.rs:classify_default` - fetch + compare + the ahead/diverged refusals now live in one function the real run and `finish -n` share, so the dry-run refuses exactly where the real run does, with byte-identical messages.
- `MergedTip` + `classify_merged_tip` - `src/release/finish.rs` - the dry-run applies the real run's ladder order to `origin/<default>`'s tip without checking it out: remote tag (`git ls-remote`, read-only) at the tip -> Released, elsewhere -> MissedBump; then `git tag --points-at <tip>` -> LocalTag (resume); a local tag of that name elsewhere -> MissedBump; else Untagged.
- New `git::tags_at` (`git tag --points-at <sha>`) - `src/git.rs` - the doc's named primitive; two unit tests (tag on a non-checked-out commit, unknown rev errors).
- The tip's version is read with the existing `git::manifest_version_at` (`git show <sha>:<manifest>`), and the sha with `git::rev_parse(origin/<default>)` after `fetch_branch`.
- Missed bump and "no manifest version at the tip" refuse in the dry-run (exit 1, the real run's `missed_bump` message), same as Phase 6's plain-bump dry-run running the real refusal.
- Printed steps follow the state: the `git checkout` line only for CheckoutHere, the `git pull --ff-only` line only when local default is behind, the CI wait / tag / push block only for Untagged (LocalTag prints it with "local tag already present"), and Released prints only the already-released line plus install/verify.
- `ReleaseReport.resumed` is true on a LocalTag dry-run, mirroring the real run's report.
- Binary tests in a new `tests/finish.rs`: released prints no `git tag -a`/push/CI wait; `--no-install` on released; pending t5 prints tag and push; local-only tag prints push without `git tag -a`; a clone behind origin with a stale `origin/main` is classified by origin's fetched tip (pull line, v0.1.2, no v0.1.1); `finish -n` leaves HEAD, `for-each-ref`, status and Cargo.toml identical on t4 and t5; missed bump refuses; Phase 7's empty-install refusal and Phases 8/9's verify lines on both states. Break-the-code: on the old `finish.rs` the six state tests fail, the three preservation tests pass (they pin behavior that must not change).
- `tests/subdir.rs:finish_from_a_subdir_runs_on_the_root` inverted: its `released_repo` is the missed-bump shape, so the dry-run now refuses (exit 1, `no untagged version on main`) from the subdir and the root alike, where it used to print a plan containing `v0.1.1`.
### Deviations
- Already released with no install command (`--no-install`, or nothing installable) prints `[dry-run] already released v<X>; would run: nothing`, not the doc's `would run: install, verify`, which would be false there. The doc's exact line prints whenever an install command resolves.
- `finish -n` refuses (exit 1) on a missed bump and on an ahead/diverged local default instead of printing steps; the doc says "prints only the steps that would run" and the real run runs none there.
- Added a `[dry-run] fetched origin/<default>: <sha8> carries <tag>` line and a one-line state header for the untagged / local-tag cases; the doc names only the already-released line.
### Tradeoffs
- Install command and verify target still resolve from the resolved worktree's on-disk `bump.yml` / `Cargo.toml` (as before) vs reading them at the merged tip with `config::load_at` - the real run reads the working tree after the fast-forward (so an untracked `bump.yml` counts there); `load_at` would diverge on that case, and the working-tree read keeps Phase 7's check on the same config it already validated.
- The dry-run fetch moves `origin/<default>` (the only ref it touches) vs `--dry-run` fetch or no fetch - the doc's contract is "fetches (still changes nothing)"; measured that `git fetch origin <branch>` does not auto-follow tags, so no local tag appears.
### Open questions
- None.

## Phase 12: Help text and README (C5, C7)
### Design decisions
- Root help renders the log path through `log_path_for_help()` - `src/cli.rs:get_tool_validation_help` - the same call the release and finish after-help already use, so all three print the resolved path.
- `--message` help reads "Commit message to use (when a clean tree amends an unpushed commit, it replaces that commit's subject)" - `src/cli.rs:Cli::message` - names the amend behavior pinned by `amend_path_honors_explicit_message`.
- New unit tests `test_every_after_help_prints_the_resolved_log_path` and `test_message_help_names_the_amend_behavior` - `src/cli.rs` - pin both help changes.
- README describes what Phases 1-11 shipped: the subdirectory walk-up, its stderr notice, the `(ignoring ...)` suffix and the generic-root refusal; `[DIR]`; the gated hand-off to `/pr:create` (already present, kept); the seven verify skip reasons in check order; `--gates` exit codes and the `--gates` / `--no-verify` and `--skip-member` conflicts; empty-install refusal; the S2/S3/S4 messages; Phase 7's dirty-tree file list and the finish generic-repo refusal; Phase 11's `finish -n` including the `would run: nothing` variant; Phase 6's dry-run refusal and lockfile listing.
### Deviations
- Doc amendment: Phase 12's bullet said "six verify skip reasons"; amended to "seven" in the design doc. The doc's own Data Model lists seven (shell syntax, not cargo install, custom root, not this repo, outside repo, no binary name, no clap dependency), and Phase 8 shipped seven (Phase 8's open question flagged it). README documents seven.
- README does not change the scenario-3 example output ("Committed and tagged"): that example stages files, so the line is still true.
### Tradeoffs
- Documenting the shell-syntax character set in the README vs. only naming the reason: the set is the contract a caller needs to predict a skip, and it names no counts or versions that drift.
### Open questions
- None.

## Implementation audit fixes (round 1)
### Design decisions
- `--gates` report: `Repo:   ` and `Gates:  ` back at their v0.4.7 padding, `Default: ` kept - `src/main.rs:report_gates`, `tests/gates.rs`, `tests/subdir.rs`, README - `/plan:release` matches `Gates:  none (ungated)` whole; new test `ungated_line_is_byte_identical_to_v0_4_7` pins the line. This reverts the "Phase 10 (superseding note)" re-pad; the original Phase 10 tradeoff stands.
- Shell-syntax set gains unquoted `*`, `?`, `[` anywhere and `#`, `~` at the start of a word - `src/lang/cargo.rs:SHELL_SYNTAX_CHARS`, `WORD_START_SYNTAX_CHARS`, `split_command_words` - "start of a word" is the splitter's own `!in_word`, so `a#b` and `a~b` stay literal as in `sh`, and a quoted or escaped `"~/x"`, `'*'`, `\*` never reaches the check.
- `finish -n` fetches with `git::fetch_branch_no_tags` (`git fetch --no-tags origin <default>`) - `src/git.rs`, `src/release/finish.rs:finish_dry_run` - the fetch moved out of `classify_default` into its two callers, so the real run keeps `fetch_branch` and only the preview changes.
- `config_at_merged_tip` - `src/release/finish.rs` - the `bump.yml` the real run reads after its move: at the tip already -> worktree file; committed at the tip -> `config::load_at(tip)`; tracked at HEAD but absent at the tip -> defaults (the move deletes it); else the worktree file (untracked or absent). Phase 7's empty-install check also runs on that config in the dry-run, as the real run re-checks after its fast-forward.
- CI line: new `CiPreview` (`Wait`, `SkippedByFlag`, `DeclaredNone(sha)`) - `src/release/tag.rs:echo_tag_steps` - `finish -n` reads `config::load_at(tip).ci`, the same committed-only read `wait_for_green` makes, and prints `[dry-run] CI gate: skipped, bump.yml at <sha8> declares `ci: none`` instead of the wait. The release dry-runs pass `CiPreview::from_gate`, unchanged output.
- C6 comment in `finish` reworded: finish refuses a generic repo without probing the gate.
### Deviations
- The doc's shell-syntax amendment and the README updates ride a fixup of the Phase 12 commit, not Phase 8: the design doc and README sections were first committed in Phase 12, so a Phase 8 fixup touching them could not apply.
- The `bump.yml` read at the tip applies whenever the checkout is not at the merged tip (behind, and the check-it-out-here case), not only "behind": the real run moves the worktree to the tip in both, so the same rule holds. Same effect, correct seam.
- `finish -n` under `ci: none` prints a one-line CI skip rather than nothing, mirroring the real run's skip notice and the existing `--no-ci-gate` preview line. No wait line prints.
### Tradeoffs
- `--no-tags` only on the preview fetch vs on every `fetch_branch`: the real run's tag import under `tagOpt=--tags` is outside the audit finding and the doc; changing it would widen the fix.
- The CI skip reads `bump.yml` committed at the tip (as the real run) vs the effective install config (which can be an untracked file): an untracked `ci: none` does not skip the real run's gate, so the preview must not either.
- Install/verify still read `Cargo.toml` from the worktree (verify target, bin name); only `bump.yml` is read at the tip. The audit scoped the fix to `bump.yml`; the ecosystem-swap case stays deferred.
### Open questions
- The ungated `bump release -n` resume path (pending version, HEAD known) still prints the CI wait under a committed `ci: none`; the same `CiPreview::DeclaredNone` read would fix it. Fold into a follow-up?
- Branch deletion (`leave-pr-to-pr-create`, `release-verbs-take-dir`, local `-D` and origin) is still outstanding, for after main is pushed.
