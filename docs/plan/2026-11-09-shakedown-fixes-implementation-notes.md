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
