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
