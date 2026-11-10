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
