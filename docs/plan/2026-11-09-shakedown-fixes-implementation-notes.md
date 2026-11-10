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
