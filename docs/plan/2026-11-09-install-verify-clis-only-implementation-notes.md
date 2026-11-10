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
