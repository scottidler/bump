# Design Document: Install and verify CLIs only

**Author:** Scott Idler
**Date:** 2026-11-09
**Status:** Draft
**Review Passes Completed:** 4/5 (panel round 1 folded in; see Resolved Decisions)

## Summary

`bump release` / `bump finish` end with an install step that has fired on every Cargo
package with a binary target since the verbs existed. That set includes the fleet's four
web servers, which it has been dropping into `~/.cargo/bin` for nothing. v0.4.6 added a
post-install `<bin> --version` check, which turned that harmless waste into a failed
release: `valet --version` is a server start, not a version print. This doc makes install
and verify a CLI-only step, keyed on the one signal that separates the fleet exactly (a
`clap` dependency), gives verify a timeout and correct advice, and makes verify follow a
configured `cargo install --path <dir>` into its member crate.

## Problem Statement

### Background

- `bump release` (ungated) and `bump finish` (gated, post-merge) both end with
  `run_install` (`src/release.rs`): resolve the install command, run it.
- Resolution (`resolve_install`): `--install <cmd>` > `bump.yml` `install:` >
  `cargo install --path .` when the root `Cargo.toml` has `[package]` and a binary target >
  skip. v0.4.6 added the binary-target half and the library skip reason.
- v0.4.6 also added `Installer::verify(bin, tag)`: run the installed binary with
  `--version`, require the tag as a whole token. Motivation, both from Scott's own
  incidents: a stale build.rs cache installed a binary reporting v0.4.0 for tag v0.4.1
  (2026-09-28); an older okta-cli earlier on PATH shadowed a fresh install (2026-11-09).
- The build-version contract (`tatari-tv/build-version-rs`, SRE-4429): `BUILD_VERSION` is
  the bare `vX.Y.Z` when HEAD is the tagged commit, `vX.Y.Z-N-g<sha>` past it. Every
  fleet CLI reads it through clap `version = env!("BUILD_VERSION")`.
- Review panel round 5 on v0.4.6 (2026-11-09, `~/.cache/review-panel/runs/MVjkXw6H/`)
  found the regression below plus the cheap wins this doc folds in.
- Industry check (scour, 2026-11-09, twelve lanes): no Rust release tool (cargo-release,
  release-plz, dist) installs or runs the artifact after release, and none distinguishes
  a CLI from a service. The install and verify steps are bump's own; the ecosystem has no
  precedent to copy for "is this a CLI".

### Problem

- **bump installs web servers.** valet, catalog-api, rust-axum-svc and rust-catalog each
  have `[package]` and `src/main.rs`, so the default install fires. Nothing uses those
  binaries in `~/.cargo/bin`; services deploy as images and are proven with `sdv probe`.
- **v0.4.6 verify fails or hangs on them.** None depends on clap. `valet --version`
  ignores the flag and starts the server: exit 1 without `VALET_STATE_SIGNING_KEY`
  (measured by the panel), or runs forever with it. `ShellInstaller::verify` uses
  `Command::output()` with no timeout. Either way the next valet `bump finish` fails after
  the tag is pushed.
- **clyde is never verified.** Its root is a virtual workspace with
  `install: cargo install --locked --path clyde`. `install_bin_name(root)` is `None`, so
  verify prints `skipped (no binary name in Cargo.toml)` for the one repo whose install
  command names exactly where the binary is.
- **The verify error gives wrong advice.** It says "fix the build and re-run `bump
  finish`". A source fix moves HEAD past the tag, which `bump finish` refuses as a missed
  bump. The "older binary earlier on PATH" hint fires even when verify ran the
  install-root path directly, where PATH played no part. The "build.rs is not
  `build_version::emit()`" hint claims more than a bare `X.Y.Z` proves.
- **Two doc statements overstate the contract.** README and the `Installer::verify` doc
  comment say the bare `vX.Y.Z` comes only from the tagged commit. build-version's rows 1,
  4 and 8 (outside CI, or no tag reachable) also emit it.
- **Prose in the claude repo still carries the old world.** release-driver's probe loop
  is `grep -q <EXPECTED-VERSION>`, a substring match that passes on `v0.2.2-3-gabc` and
  `v0.2.20`; its `git ls-remote --tags origin | grep vX.Y.Z` has the same flaw.
  `skills/plan/skills/release/SKILL.md` still teaches `GIT_DESCRIBE`.

### Goals

- bump's default install never installs or runs a web server; an explicit `--install` or
  `bump.yml` `install:` stays the owner's call. Owner: Scott ("what the fuck is sdv for?",
  2026-11-09).
- Install and verify stay for CLIs, with the verify that caught the two stale-binary
  incidents. Owner: Scott (verify requested 2026-11-09 as part of the SRE-4429 adaptation).
- One signal decides CLI vs not, with zero per-repo config. Owner: Scott (clap, after the
  29-repo scan, 2026-11-09).
- verify can never hang a release. Owner: panel round 5 must-fix 1.
- verify follows `cargo install --path <dir>` so clyde is checked. Owner: panel cheap-win 4.
- The verify error tells the truth and names the right re-run. Owner: panel cheap-win 5.
- release-driver and the plan release skill match the shipped contract. Owner: panel
  must-fix 2 and 3.

### Non-Goals

- Services answering `--version`. Proposed as option C, rejected by Scott 2026-11-09:
  nobody runs a web server with `--version`; `sdv probe /version` is the service proof.
- A Cargo metadata key (`[package.metadata.bump]`) marking CLIs. Excluded: a second signal
  for a fact clap already encodes.
- A per-repo `verify: false` switch in `bump.yml`. Excluded: four service repos would each
  need a PR to opt out of something bump should never have done to them.
- Parsing `--root` or `--bin` on a custom `cargo install` command. Parked: no fleet
  `bump.yml` uses either (v0.4.6 panel defer 8; round 1 D1). Revisit when one does. A
  quoted `--path "dir"` is no longer parked: the install command is split with shell
  quoting (implementation audit, round 2 cheap-win 1), so it resolves like an unquoted one.
- Killing the process group on a verify timeout. Parked: with stdout and stderr in files
  (below) an orphaned grandchild cannot block anything (round 1 D2).
- `autobins = false` in `has_bin_target`. Parked: zero fleet uses (panel defer 9).
- Several `[[bin]]` entries: the first wins. Parked: no affected repo (panel defer 10).
- Dropping the install step to match the ecosystem norm. Excluded: Scott's CLIs are
  installed onto this machine by the release itself, and the two stale-binary incidents
  are the reason verify exists.

## Proposed Solution

### Overview

- A CLI is a root (or install-target) package that has a binary target AND depends on
  `clap`. Across the 29 fleet repos surveyed 2026-11-09 every CLI matches and none of the
  four web servers does; Dockerfiles sit on both sides (whitespace, marquee, persona have
  one), so they decide nothing.
- The default install requires that. Anything else with a `Cargo.toml` prints a specific
  skip reason: virtual workspace root, library crate, or no clap dependency.
- verify runs after any install whose command is a `cargo install` of this repo, against
  the directory that command installs from (`--path <dir>` or `--path=<dir>`, else the
  repo root), and only when that directory names a binary and depends on clap. Any other
  install command skips verify with a reason.
- verify spawns `<bin> --version` with stdout and stderr in files and a hard timeout, and
  kills it on expiry.
- The error on mismatch names the shape it saw and the correct re-run.
- A companion commit in `scottidler/claude` fixes the two release-driver greps and the
  last `GIT_DESCRIBE` sentence.

### Architecture

Existing seams, no new modules:

- `src/lang/cargo.rs`: pure manifest and tree inspection. Gains `has_clap_dependency(dir)`
  and `cargo_install_target(dir, command) -> Option<PathBuf>` (both drafted, uncommitted,
  with tests).
- `src/release.rs`: `resolve_install`, `install_skip_reason`, `run_install`,
  `echo_install`, the `Installer` port, `ShellInstaller`. `ShellInstaller` gains
  `verify_timeout: Duration` beside its existing `install_root`.
- `src/release/finish.rs`: call sites only; already passes the tag.
- Tests: `src/release/tests/install.rs` (pure resolution and verify), `tests.rs`
  fixtures (`RecordingInstaller` gains a one-shot verify failure), `tests/finish.rs`
  (fail-then-reinstall).

### Data Model

The verify target, resolved once per install:

```
VerifyTarget = Verify { dir: PathBuf, bin: String }
             | Skip(reason)        // one of the three reasons below
```

Reasons, printed as `verify: skipped (<reason>)`, checked in this order:

1. `install command is not cargo install` (`make install`, a script)
2. `install command does not install this repo` (a positional crate name, `--git`, or
   `--registry`)
3. `no binary name in <dir>/Cargo.toml` (virtual root with no `--path`, `src/bin/` only)
4. `no clap dependency in <dir>/Cargo.toml` (a server installed by an explicit command)

Binary name before clap, because `has_clap_dependency` needs `[package]` too: the other
order would report a virtual root as "no clap" (round 1 W1).

Install skip reasons, printed as `install: skipped (<reason>; pass --install or set
install: in bump.yml)`, checked in this order on an `Auto` choice with a `Cargo.toml`:

1. no `[package]`: `virtual workspace root`
2. no binary target: `library crate, no binary target`
3. no clap: `no clap dependency: not a CLI`

### API Design

```rust
// src/lang/cargo.rs
pub fn has_clap_dependency(dir: &Path) -> bool;            // [package] + [dependencies].clap
pub fn cargo_install_target(dir: &Path, command: &str) -> Option<PathBuf>;
// "cargo install --locked --path clyde" -> dir/clyde; "--path=clyde" same; "cargo install" -> dir;
// "make install" -> None (not cargo install); "cargo install ripgrep" / "--git" / "--registry" -> None
// (does not install this repo)

// src/release.rs
pub trait Installer {
    fn install(&self, dir: &Path, command: &str) -> Result<()>;
    fn verify(&self, dir: &Path, bin: &str, tag: &str) -> Result<()>;  // dir: where the bin's manifest lives
}
pub struct ShellInstaller { pub install_root: Option<PathBuf>, pub verify_timeout: Duration }
// Default: install_root from CARGO_INSTALL_ROOT > CARGO_HOME > ~/.cargo; verify_timeout 10s
```

`run_install` after a successful install:

```
match cargo_install_target(dir, &cmd) {          // Option<Result<PathBuf, SkipReason>> in code
    not cargo install          => "verify: skipped (install command is not cargo install)",
    installs something else    => "verify: skipped (install command does not install this repo)",
    Some(target) => match install_bin_name(&target) {
        None => "verify: skipped (no binary name in <target>/Cargo.toml)",
        Some(bin) if !has_clap_dependency(&target) => "verify: skipped (no clap dependency in <target>/Cargo.toml)",
        Some(bin) => installer.verify(&target, &bin, tag)?,
    },
}
```

`ShellInstaller::verify`:

- path = `<install_root>/bin/<bin>` when that file exists, else bare `<bin>` (PATH);
  remember which.
- stdout and stderr each go to a fresh file under `std::env::temp_dir()` (never a pipe:
  a pipe nobody drains can block the child, and a descendant that inherits the pipe can
  block the post-exit read with no deadline; a file has neither problem). stdin null.
- spawn `<path> --version`; poll `try_wait` every 100ms until `verify_timeout`; on expiry
  `kill` then `wait` (reap the child), then bail. `Child::kill` on a child that exited in
  the gap returns `Ok(())` (std docs), so there is no race to handle. Descendants that
  outlive the child are not killed and cannot block (files), so the deadline is hard.
- read both files after exit (or after the kill). Every verify failure, timeout, non-zero
  exit, spawn failure and mismatch, ends with the same closing line:
  `The tag is pushed and stands. A source fix is a new release: commit it and run `bump
  release`. To only reinstall, re-run `bump finish` (repeat any --install override).`
- timeout: `install verification failed: `<path> --version` did not exit within 10s; a CLI
  answers --version at once.` plus whatever stdout and stderr held.
- non-zero exit: the status, stdout and stderr.
- `version_matches(stdout, tag)`: whitespace-token equality, unchanged.
- mismatch: `install verification failed: `<path> --version` printed "<out>", expected
  <tag>.` then one line of fact, `verify ran <path> (from the install root | from PATH)`,
  then at most one shape hint:
  - a token starts with `<tag>-` and contains `-g`: `The binary was built past the tag
    (a -N-g<sha> build): HEAD is not the tagged commit.`
  - a token equals the tag without `v`: `The binary reports CARGO_PKG_VERSION (no v), which
    the build-version contract never prints.`
  - otherwise no hint: the fact line says which path ran, and the cause (an older copy, an
    install that wrote elsewhere, a fresh build reporting the wrong thing) is for the
    reader; bump does not guess (round 1 W4).

Dry-run echo (`echo_install`) prints the same `install:` and `verify:` lines prefixed
`[dry-run]`, with `verify: <bin> --version == <tag>` when verify would run.

### Implementation Plan

Ship order: Phases 1-3 in `scottidler/bump`, one commit each, then one release; Phase 4
in `scottidler/claude` after that release so the prose describes a shipped bump.

#### Phase 1: The clap gate on the default install
**Model:** sonnet
- Commit the drafted `has_clap_dependency` with its tests (`src/lang/cargo.rs`). The
  drafted `cargo_install_target` waits for Phase 2, where it gets a caller: committing it
  here would fail `otto ci` on dead code.
- `resolve_install` `Auto` default requires `has_package_table && has_bin_target &&
  has_clap_dependency`.
- `install_skip_reason` gains the third reason in the order above.
- The shared fixture `write_cargo` (`src/release/tests.rs`) gains `[dependencies]
  clap = "4"`, so every existing release and finish test that expects the default install
  keeps expecting it; a new `write_service_cargo` fixture writes `[package]`,
  `src/main.rs` and `axum` with no clap (round 1 M1).
- Tests in `src/release/tests/install.rs`: the service fixture skips with the clap reason
  and prints it; the clap fixture still gets the default; `--install` and config
  `install:` still fire on the service fixture (only the bare default is withheld).
- **Success criteria:** `sh -c 'cd ~/repos/tatari-tv/valet && cargo run --manifest-path
  ~/repos/scottidler/bump/Cargo.toml -q -- finish -n'` prints `[dry-run] install: skipped
  (no clap dependency: not a CLI; pass --install or set install: in bump.yml)` and no
  `verify:` line; the same command in `~/repos/tatari-tv/sdv` still prints `[dry-run]
  install: cargo install --path .`. (The freshly built binary, since the installed bump
  stays v0.4.6 until the release.)

#### Phase 2: verify follows the install target, times out, and tells the truth
**Model:** opus
- Commit the drafted `cargo_install_target` with its tests, extended to accept
  `--path=<dir>`, and to return the "does not install this repo" skip for a positional
  crate name, `--git` or `--registry` (round 1 C2).
- `Installer::verify` takes the target dir; `run_install` and `echo_install` resolve the
  target per the API section; the four verify skip reasons in the stated order.
- `ShellInstaller { install_root, verify_timeout }`, `Default` fills both; `main.rs`
  unchanged (`ShellInstaller::default()`).
- `installed_bin_path` returns the path and whether it came from the install root; the
  file-backed spawn-poll-kill loop; the fact line, the two shape hints and the closing
  line on every failure.
- `RecordingInstaller` records `verify:<full target path>:<bin>:<tag>` (the full path
  tells worktrees apart, round 1 W8) and gains `failing_verify_once()`.
- Tests: clyde-shaped fixture (virtual root with `[workspace.package] version`, member
  `clyde/` with `version.workspace = true`, clap and `src/main.rs`, `bump.yml` `install:
  cargo install --locked --path clyde`) records a verify against `<work>/clyde`; the same
  virtual root with `install: cargo install` (no `--path`) skips with the no-binary-name
  reason, not the clap reason (W1); `make install` skips with the not-cargo reason;
  `cargo install ripgrep` skips with the not-this-repo reason; a fake binary that sleeps
  past a 200ms `verify_timeout` fails with "did not exit"; a fake that exits 3 with text on
  stderr fails with that text in the message; each shape hint and the fact line from fake
  output; the recovery path two ways: (a) `finish` on a released repo with
  `failing_verify_once()` returns the verify error and a second `finish` with a plain
  installer installs and verifies again; (b) ungated `release` with `failing_verify_once()`
  returns the verify error with the tag already on origin, then `finish` installs and
  verifies, with no new commit and no new push recorded (round 1 C1).
- **Success criteria:** `cargo test --bin bump -- kills_a_binary_that_does_not_exit` and
  `cargo test --bin bump -- finish_reinstalls_after_a_failed_verify` each report
  `1 passed`; `sh -c 'cd ~/repos/tatari-tv/clyde && cargo run --manifest-path
  ~/repos/scottidler/bump/Cargo.toml -q -- finish -n'` prints `[dry-run] verify: clyde
  --version == v<clyde workspace version>` (clyde's member inherits
  `[workspace.package] version`).

#### Phase 3: Docs in the bump repo
**Model:** sonnet
- README install paragraph: three install skip reasons, the verify target rule, the three
  verify skip reasons, the timeout, the re-run advice; drop "only on the tagged commit".
- `Installer::verify` doc comment: same correction (rows 1, 4, 8 also print the bare tag).
- **Success criteria:** `! rg -q 'only on the tagged commit|only when HEAD is the tagged
  commit' README.md src/release.rs` exits 0 (no match in either file); `rg -q 'no clap
  dependency' README.md` exits 0. (`rg -c` prints nothing and exits 1 on zero matches, so
  "prints 0" can never pass; round 1 M3.)

#### Phase 4: Companion prose in scottidler/claude
**Model:** sonnet
- `HOME/.claude/agents/release-driver.md` and the unstaged copy
  `HOME/.claude/skills/plan/agents/release-driver.md`: step 5's remote-tag check becomes
  `git ls-remote --exit-code --tags origin refs/tags/vX.Y.Z`; step 6's probe loop
  becomes a precheck plus a capped loop that compares the whole value, not a line:

  ```bash
  command -v sdv jq >/dev/null || { echo "sdv and jq are required"; exit 1; }
  for i in $(seq 1 40); do
    out=$(sdv probe --format json <DEPLOY-URL>) || { echo "$out"; echo "sdv probe failed (exit $?): fix auth, then re-run"; exit 1; }
    echo "$out" >> $TMPDIR/probe.log
    jq -e --arg v <EXPECTED-VERSION> '.version | select(.state == "ok") | .payload.version == $v' <<<"$out" >/dev/null && break
    sleep 30
  done
  ```

  (shape from sdv `report.rs`: `#[serde(tag = "state")]`, `Ok { payload }`; valet's
  `/version` payload carries `version`). A missing tool or an sdv failure ends the loop,
  which is what the existing "fix auth and retry once" step needs; 40 attempts is the
  patience bound the step already describes in prose (round 1 W5, W7).
- `HOME/.claude/skills/plan/skills/release/SKILL.md`: the `GIT_DESCRIBE` sentence becomes
  the build-version paragraph already in `skills/bump/SKILL.md`.
- `HOME/.claude/skills/bump/SKILL.md`: one sentence naming the three install skip reasons
  and that services are never installed.
- Commit only these files; the plan-agents copy stays unstaged if another session's edits
  are still in it, and the doc's implementation notes say so.
- **Success criteria:** in `~/repos/scottidler/claude`, `! rg -q 'grep -q <EXPECTED-VERSION>'
  HOME/.claude/agents/release-driver.md` exits 0 and `! rg -q GIT_DESCRIBE
  HOME/.claude/skills/plan/skills/release/SKILL.md` exits 0.

## Acceptance Criteria

- [ ] Every service repo skips: `for r in valet catalog-api rust-axum-svc rust-catalog; do
  sh -c "cd ~/repos/tatari-tv/$r && bump finish -n" | grep -E '^\[dry-run\] (install|verify)'; done`
  prints four `install: skipped (no clap dependency: not a CLI; ...)` lines and zero
  `verify:` lines.
  Observed on main (bump v0.4.6): each prints `[dry-run] install: cargo install --path .`
  followed by `[dry-run] verify: <bin> --version == <tag>` (valet v0.2.2, catalog-api
  v0.1.0, rust-axum-svc v0.1.1, rust-catalog bin `catalog` v0.1.0).
- [ ] clyde is verified: `sh -c 'cd ~/repos/tatari-tv/clyde && bump finish -n' | grep verify`
  prints `[dry-run] verify: clyde --version == v<version>`.
  Observed on main: `[dry-run] verify: skipped (no binary name in Cargo.toml)`.
- [ ] CLIs and libraries are unchanged: sdv prints `[dry-run] install: cargo install --path .`
  then `[dry-run] verify: sdv --version == v<sdv Cargo.toml version>`; renew prints
  `[dry-run] install: skipped (library crate, no binary target; pass --install or set
  install: in bump.yml)`.
  Observed on main: exactly those lines, with sdv at v0.5.11 when measured (the version
  moves; the criterion is the shape, round 1 D3).
- [ ] verify cannot hang and the reinstall path works: `TMPDIR=/media/saidler/4tb/claude-tmp
  cargo test --bin bump -- kills_a_binary_that_does_not_exit finish_reinstalls_after_a_failed_verify`
  reports `2 passed`.
  Observed on main: `0 passed; 0 failed; 375 filtered out` for each name (the tests do not
  exist). Baseline suite: 375 pass with that TMPDIR; the sandbox's `/tmp/claude-1000`
  symlink fails four worktree-path tests regardless of this change.
- [ ] The claude prose is exact-match: in `~/repos/scottidler/claude`,
  `! rg -q 'grep -q <EXPECTED-VERSION>' HOME/.claude/agents/release-driver.md` exits 0 and
  `! rg -q GIT_DESCRIBE HOME/.claude/skills/plan/skills/release/SKILL.md` exits 0.
  Observed on main: `rg -n` shows one hit at line 198 for the probe grep (plus line 180,
  the ls-remote grep), and `rg -c GIT_DESCRIBE` prints `1`.

## Resolved Decisions

- 2026-11-09, Scott: bump never installs or runs a web server; services are proven by
  `sdv probe`. Closes panel must-fix 1 and the A/B/C framing.
- 2026-11-09, Scott (after the 29-repo scan): the CLI signal is a `clap` dependency.
  Dockerfile/helm rejected (present on CLIs too); services answering `--version`
  rejected; a metadata key and a `bump.yml` opt-out not pursued.
- 2026-11-09, Scott: the verify step stays for CLIs (two stale-binary incidents), despite
  no ecosystem precedent (scour, twelve lanes).
- 2026-11-09, panel round 1 (`~/.cache/review-panel/runs/0QlPmZSK/synthesis.md`): all
  three must-fixes (test fixture fallout, file-backed stdout so the timeout is hard, the
  `rg -c` criteria) and every cheap win folded in; D1 and D2 parked in Non-Goals; D3
  folded into criterion 3. Two architect claims refuted by the lead with measurements
  (`Child::kill` after exit is `Ok`; libtest accepts two filters) and not acted on.

## Alternatives Considered

### Alternative 1: Dockerfile or helm/ as the service signal
- **Description:** skip install when the repo root has a `Dockerfile` or `helm/`.
- **Pros:** names the thing that deploys.
- **Cons:** whitespace, marquee and persona ship a Dockerfile and are clap CLIs bump must
  install.
- **Why not chosen:** does not separate the fleet.

### Alternative 2: Services answer `--version` with BUILD_VERSION
- **Description:** four services gain a `--version` early-exit; bump verifies everything.
- **Pros:** one rule for all binaries.
- **Cons:** installs web servers locally for no reason; adds a CLI contract to things
  that are not CLIs; four service PRs plus the template.
- **Why not chosen:** rejected by Scott 2026-11-09: a web server is not run with
  `--version`; `sdv probe` exists for this.

### Alternative 3: Explicit Cargo metadata (`[package.metadata.bump] install = true`)
- **Description:** CLIs opt in, like dist's `[package.metadata.dist]`.
- **Pros:** the one precedented shape in the ecosystem.
- **Cons:** one commit per CLI repo (20+); a second signal for a fact clap already states.
- **Why not chosen:** zero-config inference is exact on this fleet.

### Alternative 4: Drop install and verify (the ecosystem norm)
- **Description:** bump stops at the tag; the user installs by hand.
- **Pros:** matches cargo-release, release-plz, dist.
- **Cons:** the two stale-binary incidents recur with no mechanism against them; every
  CLI release gains a manual step.
- **Why not chosen:** bump's install is how Scott's CLIs reach his machine; verify is the
  structural fix for the incidents.

## Technical Considerations

### Dependencies
- `toml_edit` (present) for manifest inspection. No new crates: the timeout is
  `std::process::Child::try_wait` plus `std::thread::sleep`.
- Fleet facts the gate depends on (lead's scan of ~100 root manifests across tatari-tv
  and scottidler, round 1): every root package with `src/main.rs` depends on `clap` except
  the four services; libraries with clap (renew, mcp-io-rs) have no bin target, so the
  library reason fires first. Workspace-inherited `clap = { workspace = true }` counts:
  the key is present. Two server members do depend on clap, `marquee/server` and
  `persona/server`; their roots are virtual and no `bump.yml` targets them, so the default
  never fires, and an explicit install of one would verify and pass (clap exits on
  `--version` before serving). Re-check with the survey script if a new kind of binary
  appears.

### Performance
- Verify adds one process spawn capped at 10s and two small temp files. Dry-run adds two
  manifest parses.

### Security
- Verify runs the binary bump just installed, from the install root. Same trust model as
  running the install command itself.

### Testing Strategy
- Pure functions (`has_clap_dependency`, `cargo_install_target`, `version_matches`,
  hints, `installed_bin_path`) unit-tested on temp manifests.
- `RecordingInstaller` proves orchestration: which dir and bin were verified, and that a
  failed verify is a failed verb while a re-run reinstalls.
- `ShellInstaller` proven against shell-script fakes in a private install root: exact
  pass, each hint, the timeout.
- Dry-run observations on real fleet checkouts (the acceptance criteria) with the freshly
  built binary before release, and with the installed one after.
- In the Claude Code sandbox `TMPDIR` is the symlink `/tmp/claude-1000`, and four
  pre-existing worktree-path tests compare it against git's canonical path and fail. Run
  `TMPDIR=/media/saidler/4tb/claude-tmp otto ci` there; CI and a plain shell need nothing.

### Rollout Plan
- One bump release (patch) after Phase 3. The running `bump release` is the installed
  v0.4.6, so that run's verify is the old one; the new code is proven right after by
  running the acceptance dry-runs with the freshly installed binary (valet, clyde, sdv,
  renew), then one real `bump finish` on bump itself, whose already-released row
  reinstalls and runs the new verify (round 1 W2).
- Phase 4 commits in `scottidler/claude` after that release and is pushed; the files are
  symlinked into `~/.claude`, so they are live on commit.
- Services need no change. clyde needs no change. Libraries keep `--no-install` or let
  bump skip.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| A fleet CLI drops clap (hand-rolled arg parsing) and stops being installed | Low | Low | the skip line names the reason; `--install` or `bump.yml` `install:` still fires it |
| A service adds clap for a subcommand and starts being installed again | Low | Med | verify would then pass or fail honestly; the scan is cheap to re-run before adding clap to a service |
| Verify timeout too short on a cold `~/.cargo/bin` binary | Low | Low | 10s for a process that prints one line; the error names the timeout |
| `jq` or `sdv` absent, or sdv auth expired, where release-driver runs | Low | Med | the Phase 4 loop prechecks both with `command -v` and exits on an sdv failure instead of retrying, so the existing "fix auth and retry once" step runs |

## Open Questions

None.

## References

- `docs/design/2026-09-26-one-release-command.md`: the release verbs and the install step.
- `~/repos/tatari-tv/github-actions/docs/plan/2026-11-08-rust-version-from-cargo-toml.md`:
  the build-version contract and its cases table.
- Panel round 5 synthesis: `~/.cache/review-panel/runs/MVjkXw6H/synthesis.md`.
- Fleet scan 2026-11-09 (29 repos: Dockerfile / helm / clap / main.rs per repo), run from
  the SRE-4429 survey clones.
- Industry scour 2026-11-09: ruff `crates/ruff/src/version.rs`, uv-cli `src/version.rs`,
  cargo-release `docs/reference.md`, release-plz config docs, dist `simple-guide.md`,
  sharkdp's release checklist (dev.to, 2018).
