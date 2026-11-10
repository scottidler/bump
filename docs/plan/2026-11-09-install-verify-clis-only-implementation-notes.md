## Phase 1: The clap gate on the default install
### Design decisions
- `has_clap_dependency` requires a `[package]` table and a `clap` key in `[dependencies]` (workspace-inherited counts) - `src/lang/cargo.rs:has_clap_dependency` - the key is the CLI signal; a virtual root answers false so the member decides.
- The clap check sits last in `install_skip_reason`, after the virtual-root and library reasons - `src/release.rs:install_skip_reason` - matches the doc's stated order.
- `write_cargo` gained `[dependencies] clap = "4"` and a new `write_service_cargo` fixture (axum, no clap) - `src/release/tests.rs` - existing default-install tests keep their meaning.
- New tests: service skips with the clap reason, clap CLI still gets the default, `--install` and config `install:` still fire on a service - `src/release/tests/install.rs`.

### Deviations
- None.

### Tradeoffs
- `cargo_install_target` and its test were removed from the working tree for the commit and restored afterward, so it stays uncommitted for Phase 2 - avoids a dead-code clippy failure.

### Open questions
- None.

## Phase 2: verify follows the install target, times out, and tells the truth
### Design decisions
- `cargo_install_target` returns a three-variant enum `InstallTarget { NotCargoInstall, NotThisRepo, Dir(PathBuf) }` - `src/lang/cargo.rs:cargo_install_target` - each variant names one outcome; clearer at the call site than `Option<Result<PathBuf, SkipReason>>`.
- Positional detection uses a table of `cargo install` options that take a value (`CARGO_INSTALL_VALUE_FLAGS`); any other bare word is a crate name, and `--git`/`--registry` (spaced or `=`) are "not this repo" - `src/lang/cargo.rs` - `--features full --path cli` must not read `full` as a crate.
- The target dir is normalized with `components()`, so `--path .` resolves to `<repo>`, not `<repo>/.` - `src/lang/cargo.rs:cargo_install_target` - the recorded target and the skip-reason paths read as plain paths.
- One pure `verify_target(dir, cmd) -> VerifyTarget { Verify { dir, bin } | Skip(reason) }` feeds both `run_install` and `echo_install` - `src/release.rs:verify_target` - the dry run and the live run cannot disagree; the four skip reasons are checked in the data-model order.
- `installed_bin_path` returns `(PathBuf, BinSource)` with `BinSource { InstallRoot, Path }` - `src/release.rs` - the fact line's "from the install root | from PATH" comes from a named value, not a bool.
- stdout/stderr go to anonymous `tempfile::tempfile()` files (in `std::env::temp_dir()`, deleted on drop) - `src/release.rs:run_version` - `tempfile` is already a regular dependency, so no new crate and no cleanup code.
- Every verify failure (spawn, timeout, non-zero exit, mismatch) ends with the shared `VERIFY_RERUN` line; the spawn failure formats the io error inline rather than via `wrap_err`, so the top-level message still ends with that line - `src/release.rs:ShellInstaller::verify`.
- The timeout reads `within 10s`, or `within 200ms` for a sub-second timeout - `src/release.rs:format_timeout`.
- `RecordingInstaller::failing_verify_once()` records the verify, then fails it once with an `install verification failed (simulated)` error; `verified_entry(dir, bin, tag)` builds the `verify:<path>:<bin>:<tag>` string - `src/release/tests.rs`.

### Deviations
- `cargo_install_target` returns `InstallTarget`, not the doc's `Option<PathBuf>` - the doc's own `run_install` sketch needs three outcomes; same effect, correct seam.
- The clyde-shaped test sets `install:` through `Config { install }` passed to `run_install`, not a written `bump.yml` - same resolution path (`resolve_install` reads `config.install`), without the config-file loader in the test.
- Recovery test (a) `finish_reinstalls_after_a_failed_verify` starts from an untagged merged repo: the first `finish` tags, pushes, then fails on verify; the second `finish` runs the already-released row and reinstalls + verifies with no push and no new commit - covers "finish on a released repo" as the second run, and the tag-then-fail path as the first.
- The existing test `version_mismatch_hint_names_the_shape` was inverted and renamed `version_mismatch_hint_names_only_a_proven_shape`: a plain mismatch now asserts no hint (the "older binary earlier on PATH" guess is gone, per W4).
- `ungated_release_with_real_pusher_and_installer` keeps its `mkdir ... && printf ...` install command, which is not `cargo install`, so verify now skips there; its comment says so. `ShellInstaller::verify` is proven directly in `install.rs`.
- The `Installer::verify` doc comment still says the bare tag comes "only when HEAD is the tagged commit"; Phase 3 owns that correction.

### Tradeoffs
- A value-flag table vs. treating every `-`-prefixed word as boolean - the table is required to tell `--features full` from a crate name; an unlisted value flag would misread its value as a crate and skip verify with "does not install this repo" (a visible skip, never a wrong verify).
- A shell-chained install (`cargo install --path . && echo done`) is classified "does not install this repo", since `&&` reads as a positional - no fleet `bump.yml` chains one (the seven found are each a single `cargo install`); the skip line says why.
- The timeout test's fake runs `sleep 5` as a forked child (not `exec`), so the orphan keeps the output files open past the kill - proves the file-backed deadline is hard; the orphan exits on its own after 5s.

### Open questions
- None.

## Phase 3: Docs in the bump repo
### Design decisions
- The README install paragraph describes the code as built: the three install skip reasons in `install_skip_reason` order, the verify target rule (`--path <dir>` / `--path=<dir>`, else the repo root), the 10s kill, the fact line and closing re-run advice (source fix is a new `bump release`; reinstall only is `bump finish`) - `README.md` - read off `src/release.rs` rather than the doc's sketch.
- The README names no line numbers, counts or version pins beyond the existing `bump v0.4.6` example output - `README.md` - durable-doc rule.
- `Installer::verify` doc comment now says the bare tag comes on the tagged commit and also outside CI or with no tag reachable (cases-table rows 1, 4, 8) - `src/release.rs:Installer::verify`.
- The design doc is committed unchanged with Status left Draft - Phase 4 and finalization are pending.

### Deviations
- The README documents FOUR verify skip reasons, not the Phase 3 bullet's "three" - the bullet predates the Data Model's fourth reason (no clap dependency in `<dir>/Cargo.toml`), and Phase 2 shipped all four.

### Tradeoffs
- Folded the old "no binary name" sentence into the full ordered skip-reason list vs. keeping it as a separate sentence - one list, one order, matches `verify_target`.

### Open questions
- None.

## Implementation audit fold-in
Source: review panel round 2, `~/.cache/review-panel/runs/0QlPmZSK/synthesis.md` (# Round 2, cheap-wins 1 and 2) and `probes.md` beside it. Each fix is folded into the commit that owns the code: finding 1 into Phase 2, finding 2 into Phase 1, the doc/README/notes edits into Phase 3.

### Design decisions
- Finding 1 (cheap-win 1, probes 10-12): `cargo_install_target` split on whitespace and required word 2 == `install`, so three commands v0.4.6 verified now skipped verify with a false reason. Evidence, probes 10-12 on sdv with the 9f37c13 build: `--install 'cargo install --path . --config "build.jobs = 2"'` and `'cargo install --path . --features "a b"'` printed `verify: skipped (install command does not install this repo)`; `'cargo +nightly install --path .'` printed `verify: skipped (install command is not cargo install)`. v0.4.6 printed `verify: sdv --version == v0.5.11` for all three. Fix: a quote-aware `split_command_words` (single quotes literal, double quotes group with `\"`/`\\` escapes, backslash outside quotes escapes one char, quoted pieces join the word they touch) and an optional `+<toolchain>` word after `cargo` - `src/lang/cargo.rs:split_command_words`, `cargo_install_target` - no new crate, per the doc's Dependencies section.
- Tests for finding 1: `test_cargo_install_target_quotes_and_toolchain` asserts all three commands resolve to `InstallTarget::Dir("/repo")`, a quoted `--path "my cli"` and `--path='my cli'` resolve to `/repo/my cli`, and `cargo +nightly ripgrep` stays `NotCargoInstall`; `test_split_command_words_honors_quotes` covers the tokenizer's quoting, escapes, an empty quoted word and an unterminated quote - `src/lang/cargo.rs`.
- Finding 2 (cheap-win 2): the Phase 1 bullet "skips with the clap reason and prints it" was only half met; `resolve_install_auto_skips_on_service_without_clap` asserted the `install_skip_reason` string, and nothing asserted the printed line or the absence of a verify line. Fix: `release_dry_run_on_a_service_prints_the_clap_skip_and_no_verify` runs the compiled `bump release -n --no-ci-gate` on a valet-shaped fixture (`[package]`, `src/main.rs`, `axum`, no clap) and asserts the `[dry-run] install:`/`[dry-run] verify:` lines are exactly the one clap skip line - `tests/release_cli.rs` - real binary, real stdout, the same pattern as the file's existing dry-run test.

### Deviations
- The doc's Non-Goals bullet no longer parks a quoted `--path "dir"`: the splitter makes it resolve like an unquoted one. `--root` and `--bin` stay parked.
- An unterminated quote runs to the end of the command instead of producing a separate "could not parse" verify reason (the synthesis offered one) - `sh -c` rejects that line in `ShellInstaller::install` before verify runs, so only a dry run can show its verify line.
- The Phase 3 commit body says the design doc is "committed unchanged"; that commit now also carries this Non-Goals edit. The commit bodies stay as written, per the fold-in instruction.

### Tradeoffs
- A hand-written splitter vs. a `shlex` dependency - the doc says no new crates, and the install line needs only quotes and backslashes; `$var`, globs and `$(...)` are not expanded (an install line using them is not a plain `cargo install` of a path bump can resolve).
- The service-print assertion lives in a CLI integration test rather than capturing `println!` in a unit test - the unit tests have no stdout seam, and the compiled binary's stdout is what the Phase 1 success criterion measures.

### Open questions
- None.

## Phase 4: Companion prose in scottidler/claude
Commit b57a463c89dbcc6758da8f4beeb427c436c975de in `scottidler/claude` (not pushed).

### Design decisions
- Staged only this phase's hunks of the two files another session had already edited (`HOME/.claude/skills/plan/agents/release-driver.md`, `HOME/.claude/skills/plan/skills/release/SKILL.md`) by writing HEAD plus my transform as the index blob (`git update-index --cacheinfo`). The foreign hunks stay uncommitted in the working tree. No file was left wholly unstaged.
- Step 6 wait bullet keeps the doc's code block verbatim; the surrounding prose was rewritten because the old `tee`-vs-`grep -q` explanation no longer applies.
- The `GIT_DESCRIBE` sentence in the plan release skill became the build-version paragraph copied from `skills/bump/SKILL.md`, which now also carries the install-skip sentence (three reasons, services never installed by default, `--install` or `bump.yml` `install:` overrides).

### Deviations
- The `ls-remote` line gained a short "(exit 0 means the tag is there)" gloss beyond the bare command. Same effect.
- `HOME/.claude/agents/release-driver.md` and `skills/bump/SKILL.md` are inside the claude repo's `PLAN_OLD` rollback-baseline set (`.otto.yml` lint). The doc requires editing them, so `otto ci` lint stays red; see open questions.

### Tradeoffs
- Index-blob staging vs `git add -p`: the blob route is deterministic and leaves foreign hunks intact; `-p` on interleaved hunks risks mis-staging.

### Open questions
- `otto ci` in the claude repo fails its `lint` task: "The old pipeline files moved off the rollback baseline 62dc65f; fix plan/ instead", covering `agents/release-driver.md` and `skills/bump/SKILL.md`. This predates Phase 4 (HEAD already differs from the baseline by 21 lines in those two files, from commit da11994) and my edit adds to the drift. The `test` task passed. Should the baseline check exempt these two files, or is the old-pipeline soak over?
- Another session has uncommitted edits in the claude repo (including both plan files' other hunks); they remain untouched.
