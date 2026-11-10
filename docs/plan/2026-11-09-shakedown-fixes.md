# Design Document: bump v0.4.7 shakedown fixes

**Author:** Scott Idler
**Date:** 2026-11-09
**Status:** Draft
**Review Passes Completed:** 2/5 (draft, correctness; Scott's bug 1 ruling and panel round 1 folded in)

## Summary

The v0.4.7 shakedown (`docs/shakedown-v0.4.7.md`) found 7 bugs, 7 cosmetic items and 5
suggestions in bump, none in the install/verify work it was run for. Scott: "the whole
thing then, targetted fixes and all". This doc fixes every item, lands the two unmerged
branches that already fix two of them, and kills a flaky test that came in with v0.4.7.
Contract changes a caller can observe:

- bump run from a subdirectory acts on the repo root, with a notice; it refuses when the
  root has no version manifest but the subdirectory does.
- verify skips (true reason) on shell syntax, an out-of-repo `--path`, or a custom
  install root; env-prefixed `cargo install` now verifies.
- `--gates` exits 1 on UNKNOWN; `--gates --no-verify` and `--skip-member` with
  `--tag-only` / `--gates` are clap errors.
- an empty install command is refused.
- `finish -n` fetches (still changes nothing) and prints only the steps that would run.

## Problem Statement

### Background

- v0.4.7 (2026-11-09) made install and verify CLI-only
  (`docs/plan/2026-11-09-install-verify-clis-only.md`). The shakedown ran ~130 `bump`
  calls against throwaway repos and fleet dry-runs.
- `git diff --stat v0.4.6 v0.4.7 -- src` touches only `src/lang/cargo.rs`,
  `src/release.rs` and the release tests. Every bug outside those files is the same in
  v0.4.6 (re-measured on both binaries by the research pass).
- Two local branches already carry fixes and never merged:
  - `leave-pr-to-pr-create` (3208457, local only): gated `bump release` stops opening
    PRs and drops `pr_title`.
  - `release-verbs-take-dir` (eba0468, on origin): `release` and `finish` take an
    optional `[DIR]`.

### Problem

Bugs, worst first (root causes verified at d4b08a4):

1. **A subdirectory is a generic repo.** `git::is_git_repo` (`src/git.rs`) runs
   `git rev-parse --git-dir`, which succeeds anywhere in a repo; detection
   (`lang::detect`) then looks only at that directory. From `repo/src`, plain `bump`
   tags v0.1.2 with Cargo.toml still 0.1.1 and no commit; ungated `bump release -n
   --no-ci-gate` takes the pushed untagged HEAD as a generic resume and plans `git push
   origin v0.1.2`, exit 0. Same in v0.4.6 (since 0934cd1 and 5817e47).
2. **`--skip-member` swallows the directory.** `src/cli.rs` declares it `num_args = 1..`,
   pinned by `test_cli_skip_member_swallows_trailing_positional`, whose comment says the
   swallow "fails closed downstream". It does not for `--tag-only`: `process_directory`
   returns into `tag_only` before the skip list is validated. Measured: `cd t5 && bump
   --tag-only --skip-member claude-pricing <t4>` tagged t5, exit 0. Other modes fail
   closed but name the wrong directory.
3. **Gated `bump release` opens the PR itself, titled `chore: bump vX.Y.Z`.** `pr_title`
   (`src/release/pr.rs`) builds a `type(scope):` title. Breaks the routed git rule
   (bump never opens PRs) and the no-prefix PR title rule. Fixed on 3208457.
4. **`bump -n` skips the "HEAD already has a tag" check.** The dry-run block in
   `src/main.rs` prints `Would commit and tag`, exit 0; the real run refuses, exit 1.
5. **The dirty-tree refusal gives advice that cannot work.** `ReleaseState::DirtyTree`
   carries no file list and says "Commit or stash". On untracked-only files `git stash`
   answers "No local changes to save". Real case: six repos (catalog-api, rust-catalog
   and four others) show `?? target` because `target` is a symlink and the global
   ignore pattern `target/` matches directories only.
6. **An empty install command is accepted.** `--install ""` and `bump.yml` `install: ""`
   run `sh -c ""`, exit 0, and count as an install. `--standalone ""` is already refused.
7. **Wrong verify verdicts** (`cargo_install_target`, `src/lang/cargo.rs`):
   - `cargo install --path . && echo done`: `&&` reads as a crate name, "does not install
     this repo". v0.4.6 verified it.
   - `RUSTFLAGS=-g cargo install ...`, `env cargo install ...`: "not cargo install".
     v0.4.6 verified it.
   - `--path ../other`: `dir.join` with no containment check verifies another repo's
     binary against this repo's tag. Breaks the prior doc's "a visible skip, never a
     wrong verify".
   - `--path .;`: the word splitter does not separate `;`.

Found by the research pass, not the shakedown:

8. **Flaky test.** `shell_installer_verify_runs_the_installed_binary` failed 2 of 7 full
   runs with `Text file busy (os error 26)`. `fake_install_root` writes the script from
   the test process; a parallel test that forks in that window holds the write handle
   until its exec. Came in with 4158bb3 (v0.4.7). Every phase's `otto ci` inherits it.

Cosmetic (C) and suggestions (S), all from the shakedown:

- C1: generic repo prints `unknown -> X`; "Committed/Amended ... and tagged" prints when
  nothing was staged.
- C2: multi-repo `--gates` ends with "All done! Don't forget to push"; its `Branch:` line
  prints the default branch.
- C3: verify error renders "exited exit status: 3".
- C4: dry-run says "Would update: Cargo.toml" but also syncs lockfiles; two wordings for
  the same action.
- C5: root help hardcodes `~/.local/share/bump/logs/bump.log`; subcommand help prints the
  resolved path.
- C6: `finish` on a generic repo says "Gated generic repos are unsupported" without
  checking the gate.
- C7: `--message` help does not say it replaces an amended commit's subject.
- S1: `finish -n` echoes `git tag -a` on an already-released version.
- S2: install on a repo with no Cargo.toml prints a bare "skipped".
- S3: install failure has no exit status and no re-run line.
- S4: the verify fact line says "(from PATH)" without the resolved path.
- S5: `--gates` exits 0 on UNKNOWN; `--gates --no-verify` is silently ignored; `--no-tag`
  skips the gate probe.

### Goals

- Every bug, cosmetic item and suggestion above is fixed. Owner: Scott ("the whole thing
  then, targetted fixes and all", 2026-11-09).
- bump never tags, pushes or installs against a directory it misread. Owner: Scott's
  fail-closed rule (`rules/taste.md`).
- Every verify verdict is true: a visible skip, never a wrong verify. Owner: the prior
  doc's stated principle.
- The two unmerged branches land, so nothing stays orphaned. Owner: `process-taste.md`
  ("nothing gets orphaned").

### Non-Goals

- The global git ignore matching a symlinked `target`. Excluded from this doc: dotfiles,
  not bump. Bug 5's fix names the file, which makes the cause obvious. Revisit in the
  dotfiles repo.
- A shell parser for install commands (verify one segment of a compound). Excluded: see
  Alternative 2.
- Monorepo subdirectory releases (per-package tags). Excluded by the flat `v*` tag rule.
- Changing `--skip-member` to a repeated-only flag. Not pursued: see Resolved Decisions.

## Proposed Solution

### Overview

- Land 3208457 and eba0468 first, rebased onto main, so later phases build on them.
- One repo-root resolver used by every entry point (plain bump, `--gates`, `--tag-only`,
  `release`, `finish`, and their `[DIR]`): a subdirectory walks up to its repo root, with
  a one-line notice, like git, cargo and otto walk up.
- Close the `--skip-member` swallow where it does not fail closed.
- Make each refusal and dry-run say what the real run does.
- Make `cargo_install_target` return a true skip for compounds and out-of-repo paths,
  and verify env-prefixed commands.
- Fix the cosmetic items and suggestions.
- Companion prose in `scottidler/claude` after the release.

### Architecture

Existing seams, no new modules:

- `src/git.rs`: gains `repo_root(dir) -> Result<RepoRoot>` from `git rev-parse
  --show-toplevel --show-prefix`. The empty-prefix check decides "already at the root",
  not a path comparison: path comparison breaks on symlinks (the sandbox's
  `/tmp/claude-1000` already forces `canonicalize` in the finish tests).
- `src/main.rs`: the directory loop and `resolve_verb_dir` (from eba0468) call the
  resolver before anything else, and the loop dedups directories by resolved root.
- `src/cli.rs`: `--skip-member` conflicts.
- `src/lang/cargo.rs`: `split_command_words` also reports whether it met any unquoted,
  unescaped shell syntax; `cargo_install_target` recognizes one literal shape (below)
  and skips everything else.
- `src/release.rs`, `src/release/finish.rs`: messages, `DirtyTree` file list, empty
  install check, state-aware `finish -n`.

### Data Model

```rust
// src/release.rs
ReleaseState::DirtyTree { files: Vec<String> }   // `git status --porcelain` lines

// src/lang/cargo.rs
pub enum InstallTarget {
    NotCargoInstall,
    NotThisRepo,
    ShellSyntax,          // new: any unquoted, unescaped shell syntax
    CustomRoot,           // new: --root, or a CARGO_INSTALL_ROOT= / CARGO_HOME= prefix
    OutsideRepo(PathBuf), // new: --path resolves outside the repo
    Dir(PathBuf),
}
```

The one shape verify recognizes (round 1 M1), after `split_command_words`:

```
[NAME=value ...] [env [NAME=value ...]] cargo [+toolchain] install <args>
```

- Shell syntax: any unquoted, unescaped `$`, backtick, `;`, `&`, `|`, `<`, `>`, `(`,
  `)`, `*`, `?`, `[` or newline anywhere, or a `#` or `~` at the start of a word.
  Quotes and backslash escapes are the splitter's existing rules (`\;` is a literal `;`;
  `--features "a&&b"` is one quoted word; `"~/x"` and `'*'` are literal). This covers
  `&&`, `||`, a trailing `&` (whose install exit is unknowable), `2>&1`, `$(...)`, `$X`,
  globs, tilde expansion and a `#` comment (`cargo install --path . # --root /x` drops the
  `--root`). It is a recognizer, not a parser: anything outside the shape skips.
  (Amended by the implementation audit, round 1: without `#`, `~` and the glob
  characters, `--path ~/x`, `--path ./sr*` and `--path . # c` skipped for a false
  reason, which broke the goal "every verify verdict is true".)
- `NAME=value` words: `NAME` matches `[A-Za-z_][A-Za-z0-9_]*`. `CARGO_INSTALL_ROOT` or
  `CARGO_HOME` among them, or `--root` among the args, is `CustomRoot`: verify would read
  the default root and run a stale binary.
- `--path`: joined to the repo root, then both canonicalized (follows symlinks, so an
  in-repo symlink to elsewhere is caught; canonical vs canonical, so the sandbox's
  symlinked TMPDIR is not a false positive). Not under the canonical root ->
  `OutsideRepo`. A path that does not exist falls through to the binary-name skip.

Verify skip reasons, in check order (three new, marked):

1. `install command uses shell syntax; verify reads one plain cargo install` (new)
2. `install command is not cargo install`
3. `install command sets its own install root` (new)
4. `install command does not install this repo`
5. `install target <p> is outside this repo` (new)
6. `no binary name in <dir>/Cargo.toml`
7. `no clap dependency in <dir>/Cargo.toml`

### API Design

```rust
// src/git.rs
pub struct RepoRoot { pub root: PathBuf, pub prefix: String }  // prefix "" at the root
/// Err outside any work tree (bare repo, inside .git, no repo): today's error, unchanged.
pub fn repo_root(dir: &Path) -> Result<RepoRoot>;
```

Subdirectory notice, printed to stderr once per resolved directory, the same from every
entry point, before any other output:

```
bump: <dir> is <prefix> inside <root>; running on <root>
```

One shape refuses instead of walking up (Scott, round 1 M2): the root has no version
manifest (`lang::detect` says Generic) and `<dir>` or any directory between it and the
root has one. Walking up there would tag a version no file holds (tatari-tv/mpe-qa-tool:
generic root, `plugin/pyproject.toml` tracked by its `v*` tags). Exit 1, before any git
work:

```
bump: <dir> is <prefix> inside <root>, which has no version manifest, but <manifest> does;
bump runs on a repo root and will not tag a version no root file holds
```

When the root does have a manifest and `<dir>` has its own too, bump walks up and the
notice gains `(ignoring <manifest> in <prefix>)`.

Two directory arguments that resolve to one root run once (first wins, order kept).

`--skip-member`:

- `conflicts_with_all = ["tag_only", "gates"]` (neither reads it). Those were the only
  two modes where a swallowed directory did not fail closed.
- Every other mode already fails closed in `validate_project`. Its unknown-member error
  gains: `if <value> is a directory, put directories before --skip-member`. No
  directory-value check at parse time: a member name can also be a sibling directory
  (`cd ~/repos/tatari-tv && bump clyde --skip-member claude-pricing`, round 1 M3).

Empty install: an `--install` or `bump.yml` `install:` whose trimmed value is empty fails
with `install command is empty; pass --no-install to skip the install`, next to the
`--standalone` check. Precedence first, emptiness second (round 1 C2): `--no-install`
beats an empty `install:`; a non-empty `--install` beats an empty `install:`. `finish`
checks the config of the worktree it picked, not the caller's cwd.

`--gates`: UNKNOWN exits 1 after printing the same report. `--gates` with `--no-verify`
is a clap conflict.

### Implementation Plan

Ship order: Phases 1-12 in `scottidler/bump`, one commit each, then one release (bump is
ungated: `bump release` inline). Phase 13 in `scottidler/claude` after that release, so
the prose describes a shipped bump.

Fixtures for the success criteria (shell, inline here, never a file under `docs/`):

```sh
cargo build --manifest-path ~/repos/scottidler/bump/Cargo.toml   # the tree under test
FX=$TMPDIR/shake; BUMP=~/repos/scottidler/bump/target/debug/bump
mk() { F=$FX/$1; git init -q --bare $F.git; git init -q -b main $F; cd $F; git config user.email t@t; git config user.name t
  mkdir src; printf '[package]\nname = "shake"\nversion = "0.1.1"\nedition = "2021"\n\n[dependencies]\nclap = "4"\n' > Cargo.toml
  echo 'fn main() {}' > src/main.rs; git add -A; git commit -qm init; git tag -a v0.1.1 -m v0.1.1
  echo '// more' >> src/main.rs; git commit -qam more; git remote add origin $F.git; git push -q origin main v0.1.1; git remote set-head origin main; }
v2() { sed -i 's/^version = "0.1.1"/version = "0.1.2"/' Cargo.toml; git commit -qam 'v0.1.2'; }
mk sub
mk t4; v2; git tag -a v0.1.2 -m v0.1.2; git push -q origin main v0.1.2   # released, HEAD tagged
mk t5; v2; git push -q origin main                                       # pending, no tag
mk t6; touch scratch.txt; ln -s /nonexistent target                      # untracked file + symlink
mkdir -p $FX/other/src; cd $FX/other; printf '[package]\nname = "otherbin"\nversion = "9.9.9"\nedition = "2021"\n\n[dependencies]\nclap = "4"\n' > Cargo.toml; echo 'fn main() {}' > src/main.rs
G=$FX/G; git init -q -b main $G; cd $G; git config user.email t@t; git config user.name t
echo a > f; git add f; git commit -qm a; git tag -a v0.6.0 -m v0.6.0; echo b >> f; git commit -qam b
M=$FX/mq; git init -q -b main $M; cd $M; git config user.email t@t; git config user.name t   # mpe-qa-tool shape
echo readme > README.md; mkdir -p plugin/src; echo 'fn main() {}' > plugin/src/main.rs
printf '[package]\nname = "mq"\nversion = "0.2.0"\nedition = "2021"\n' > plugin/Cargo.toml
git add -A; git commit -qm init; git tag -a v0.2.0 -m v0.2.0; echo more >> README.md; git commit -qam more
```

`BUMP_GATES_PROBE=ungated|gated:pull_request` is bump's own gate-probe test seam.

#### Phase 1: Kill the flaky verify test
**Model:** sonnet
- `fake_install_root` (`src/release/tests/install.rs`) writes the fake binary from a
  short-lived child process (`install -m 755`), so the test process never holds a write
  handle on a file it later execs.
- **Success criteria:** `for i in $(seq 1 10); do TMPDIR=/media/saidler/4tb/claude-tmp
  cargo test -q --bin bump >/dev/null 2>&1 || echo FAIL; done` prints nothing (all ten
  runs exit 0, round 1 C3). Observed on main: 2 of 7 runs failed with `Text file busy
  (os error 26)`.

#### Phase 2: Land leave-pr-to-pr-create (bug 3)
**Model:** sonnet
- Rebase 3208457 onto main (174d640 drops as already applied). Resolve the additive
  conflict in `tests/release_cli.rs`: both sides append a test; keep both.
- Fix the stale prose that says bump opens the PR (`src/cli.rs` release/finish help,
  `src/main.rs` gates report) if 3208457 left any.
- **Success criteria:**
  - `cargo test --test release_cli gated_release_never_spawns_gh_pr_create` reports
    `1 passed`. Observed on main: `0 passed; ... 3 filtered out`.
  - `rg -q 'fn pr_title' src; echo $?` prints `1`. Observed on main: `0`.

#### Phase 3: Land release-verbs-take-dir
**Model:** sonnet
- Rebase eba0468 onto Phase 2 (clean onto main; `README.md` conflicts on one sentence
  when stacked: merge the two wordings). Reword its subject to drop the `feat(cli):`
  prefix to match this repo's log.
- **Success criteria:**
  - `$BUMP release --help | rg -q '\[DIR\]'; echo $?` prints `0`. Observed on main: `1`.
  - The verb uses it (round 1 C5): `sh -c "cd / && $BUMP finish -n $FX/t5" | rg -c
    'git tag -a v0.1.2'` prints `1`. Observed on main: `error: unexpected argument`,
    exit 2.

#### Phase 4: Repo-root resolver (bug 1)
**Model:** opus
- `git::repo_root`; call it first in the plain-bump directory loop (covers `--gates`
  and `--tag-only`) and in `resolve_verb_dir`. Print the notice; dedup by root.
- The generic-root refusal above, checked right after resolution, for every entry point.
- Tests: a subdirectory resolves to the root on plain bump, `--gates`, `--tag-only`,
  `release`, `finish`, and `release <subdir>`; a subdirectory with its own manifest gets
  the "(ignoring ...)" notice; `bump <root> <root>/src` runs once; the three
  `resolve_verb_dir` tests from eba0468, which use non-repo paths, are rewritten against
  real repos (round 1 Q3); a symlinked path to a
  root gets no notice; a non-repo dir keeps today's error.
- **Success criteria:**
  - `sh -c "cd $FX/sub/src && $BUMP -n --no-verify"; echo $?` prints the notice naming
    `$FX/sub`, `bump: 0.1.1 -> 0.1.2`, `Cargo.toml` in the would-update list, and `0`.
    Observed on main: `bump: unknown -> 0.1.2`, `[dry-run] Would create new commit and
    tag: v0.1.2`, `0`.
  - `sh -c "cd $FX/sub/src && BUMP_GATES_PROBE=ungated $BUMP release -n --no-ci-gate";
    echo $?` prints the notice, then the same `nothing to release` and `1` that the
    same command prints at `$FX/sub`. Observed on main: plans `git push
    --no-follow-tags origin v0.1.2`, `0` (at the root: `nothing to release`, `1`).
  - `sh -c "cd $FX/mq/plugin && $BUMP -n --no-verify"; echo $?` prints the generic-root
    refusal and `1` (fixture `mq`: root holds only `README.md`, `plugin/Cargo.toml` at
    0.2.0, tag v0.2.0, one commit past it). Observed on main: `bump: 0.2.0 -> 0.2.1`,
    `[dry-run] Would update: Cargo.toml`, `[dry-run] Would amend previous commit and tag:
    v0.2.1`, `0` (main reads `plugin/` alone, which is why the mpe-qa-tool shape works
    today).

#### Phase 5: --skip-member fails closed (bug 2)
**Model:** sonnet
- `conflicts_with_all = ["tag_only", "gates"]`; the `validate_project` hint above.
- Keep `test_cli_skip_member_swallows_trailing_positional` (the parse is unchanged) and
  correct its comment: it fails closed downstream in every mode the conflicts leave.
  Add `skip_member_conflicts_with_tag_only_and_gates` and a test that a sibling-directory
  member name (the clyde shape) is accepted.
- **Success criteria:** `sh -c "cd $FX/t5 && $BUMP --tag-only --skip-member
  claude-pricing $FX/t4"; echo $?; git -C $FX/t5 tag -l v0.1.2` prints a clap error,
  `2`, and no tag. Observed on main: `Tagged v0.1.2 on merged main`, `0`, `v0.1.2`.

#### Phase 6: Plain-bump dry-run tells the truth (bug 4, C1, C4)
**Model:** sonnet
- The dry-run runs the same `head_has_tag` refusal as the real run (and the `--force`
  amend vs new-commit choice).
- Generic repo: "from" is the latest tag; "Tagged vX" when nothing was staged.
- Dry-run lists each manifest with its lockfile; one wording for commit-and-tag.
- **Success criteria:**
  - `$BUMP -n --no-verify $FX/t4; echo $?` prints `Error: HEAD already has a tag` and
    `1`. Observed on main: `[dry-run] Would commit and tag: v0.1.3`, `0`.
  - `$BUMP -n --no-verify $G` prints `v0.6.0 -> 0.6.1`. Observed on main: `bump:
    unknown -> 0.6.1`.

#### Phase 7: Release and finish refusals (bug 5, bug 6, C6)
**Model:** sonnet
- `DirtyTree { files }`: the refusal lists the porcelain lines and says "commit them,
  `git stash -u`, remove them, or gitignore them".
- Empty `--install` / `install:` refused (API section), in `release()` and `finish()`.
- C6: "bump finish does not support generic repos".
- **Success criteria:**
  - `sh -c "cd $FX/t6 && BUMP_GATES_PROBE=ungated $BUMP release -n" 2>&1 | rg -c
    'scratch.txt|target'` prints `2` or more. Observed on main: nothing ("Commit or
    stash your changes first").
  - `sh -c "cd $FX/t4 && $BUMP finish -n --install ''"; echo $?` prints the empty-install
    refusal and `1`. Observed on main: `[dry-run] install: `, `verify: skipped (install
    command is not cargo install)`, `0`.

#### Phase 8: True verify verdicts (bug 7)
**Model:** opus
- `split_command_words` reports unquoted, unescaped shell syntax; `cargo_install_target`
  recognizes the one shape in the Data Model section and returns the three new variants.
- Unit table tests, one row per shape: `\;`, `--features "a&&b"`, `2>&1`, `$(...)`,
  `$X`, backticks, trailing `&`, `env A=1 cargo install`, `CARGO_INSTALL_ROOT=/x cargo
  install`, `--root /x`, `--path ../other`, an in-repo symlink to outside, an absolute
  path inside the repo.
- The three new skip reasons in the stated order; README updated in Phase 12.
- **Success criteria** (each in `$FX/t4` with `$BUMP finish -n --install '<cmd>'`):
  - `RUSTFLAGS=-g cargo install --path .` prints `verify: shake --version == v0.1.2`.
    Observed on main: `skipped (install command is not cargo install)`.
  - `cargo install --path . && echo done` prints the shell-syntax skip. Observed on
    main: `skipped (install command does not install this repo)`.
  - `cargo install --path ../other` prints the outside-repo skip. Observed on main:
    `verify: otherbin --version == v0.1.2`.

#### Phase 9: Install and verify messages (C3, S2, S3, S4)
**Model:** sonnet
- C3: `exited with {status}`.
- S2: `skipped (no Cargo manifest; pass --install or set install: in bump.yml)`.
- S3: install failure prints the exit status and the shared re-run line.
- The shared re-run line (`VERIFY_RERUN`) names the directory: `re-run bump finish
  <root>` (round 1 C4), since `[DIR]` lets the caller be anywhere.
- S4: resolve the bin on PATH and print the absolute path in the fact line.
- **Success criteria:**
  - `rg -c 'exited \{status\}' src/release.rs` prints nothing. Observed on main: `1`.
  - `sh -c "cd $FX/t4 && $BUMP finish --install 'exit 7'" 2>&1 | rg -c 'exit status:
    7|bump finish /'` prints `2` (status and a dir-qualified re-run line). It runs
    `finish` for real, on the fixture only. Observed on main: `Already released v0.1.2`,
    `install: exit 7`, `Error: install command failed: exit 7` (no status, no re-run
    line), so `0`.

#### Phase 10: --gates (C2, S5)
**Model:** sonnet
- No "All done!" line under `--gates`; label `Branch:` -> `Default:`.
- UNKNOWN exits 1; `--gates` conflicts with `--no-verify`.
- `--no-tag` keeps skipping the gate probe (round 1 M5): it is the gated path's own
  step (`version_commit`, `src/release.rs`), so a probe there only adds a "gated" warning
  to every gated `bump release`. `no_tag_skips_gate_probe_even_when_gated` stays. The
  shakedown suggestion is declined; see Resolved Decisions.
- **Success criteria:** `$BUMP --gates $FX/t4 $FX/t5; echo $?` prints no `All done!`
  line, then `1` (fixture remotes are not GitHub: UNKNOWN). Observed on main: `All
  done! Don't forget to push your changes.`, `0`.

#### Phase 11: State-aware finish -n (S1)
**Model:** opus
- `finish_dry_run` fetches `origin/<default>` and classifies the merged tip without
  checking it out (round 1 M4): `git rev-parse origin/<default>`, `git tag --points-at`
  on that sha, and the manifest version read with `git show <sha>:<manifest>`. Today it
  reads the current checkout (`finish.rs`), while the real run reads the merged tip.
- It prints only the steps that would run. Already released: `[dry-run] already
  released v<X>; would run: install, verify`.
- `finish` help: "execute nothing" -> "changes nothing (it fetches origin)".
- It must not drop Phase 7's empty-install refusal or Phase 8/9's verify lines.
- **Success criteria:** `sh -c "cd $FX/t4 && $BUMP finish -n"; echo $?` prints `already
  released v0.1.2`, no `git tag -a` line, and `0`. Observed on main (installed v0.4.7, t4 released): `1`; the dry-run
  prints `(only if the merged version is untagged; already released runs only the
  install:)` then `git tag -a v0.1.2 <sha>` and `git push --no-follow-tags origin
  v0.1.2` anyway.

#### Phase 12: Help text and README (C5, C7)
**Model:** sonnet
- Root help calls `log_path_for_help()`; `--message` help names the amend behavior.
- README: subdirectory walk-up and its notice, `[DIR]`, gated hand-off to `/pr:create`, seven verify skip
  reasons, `--gates` exit codes.
- **Success criteria:** `$BUMP --help | rg 'Logs are'` and `$BUMP release --help | rg
  'Logs are'` print the same path. Observed on main: `~/.local/...` vs
  `/home/saidler/.local/...`.

#### Phase 13: Companion prose in scottidler/claude
**Model:** sonnet
- `HOME/.claude/skills/plan/skills/release/SKILL.md`: both "exit(s) 0 on every verdict"
  sentences (Step 1 and the reference block) -> UNKNOWN exits 1 (Branch 5 already routes
  a non-zero exit). Both "until `bump release --help` shows `[DIR]`" sentences go.
- `HOME/repos/.claude/routed/git.md`: drop "until `bump release --help` shows `[DIR]`".
- `HOME/.claude/skills/bump/SKILL.md`: drop its `[DIR]` "until" sentence; add one
  sentence: a subdirectory runs on its repo root, with a notice.
- **Success criteria:** in `~/repos/scottidler/claude`, `! rg -q 'exits? 0 on every
  verdict|until `bump release --help` shows' HOME/.claude/skills/plan/skills/release/SKILL.md
  HOME/.claude/skills/bump/SKILL.md HOME/repos/.claude/routed/git.md; echo $?` prints
  `0`. Observed on main: `1`.

The Phase 3 second criterion ran on main against `scratch/fx/t4`: `error: unexpected
argument '<dir>' found`, exit 2.

## Acceptance Criteria

- [ ] A subdirectory run acts on the repo root: Phase 4's two commands print the notice
  and behave exactly as at the root (`0.1.1 -> 0.1.2` with Cargo.toml; `nothing to
  release`, exit 1). Observed on main: a generic `unknown -> 0.1.2` (plain) and a planned
  tag push, exit 0 (release).
- [ ] No swallowed directory gets tagged: Phase 5's command exits 2 and t5 has no
  v0.1.2. Observed on main: exit 0, t5 tagged.
- [ ] Gated release never opens a PR: `cargo test --test release_cli
  gated_release_never_spawns_gh_pr_create` reports `1 passed`, and `rg -q 'fn pr_title'
  src; echo $?` prints `1`. Observed on main: `0 passed`, and `0`. (Not `rg 'gh pr
  create'`: 3208457 keeps the phrase in a doc comment, `src/release.rs`, "it never runs
  `gh pr create`".)
- [ ] Every verify verdict is true: Phase 8's three commands print verify, the
  shell-syntax skip, the outside-repo skip. Observed on main: two false skips and one wrong
  verify.
- [ ] CI is stable: Phase 1's ten-run loop prints nothing. Observed on main: 2 of 7
  failed.

## Resolved Decisions

- 2026-11-09, Scott: bug 1 walks a subdirectory up to its repo root (option B), over the
  recommended refusal. The author adds the notice line and the "(ignoring ...)" suffix
  so the walk-up is never silent.
- 2026-11-09, Scott (panel round 1 M2, option A): the walk-up refuses when the root is
  generic and a manifest sits in the subdirectory path. Every other subdirectory walks
  up.
- 2026-11-09, author, amended by panel round 1 M3: `--skip-member` keeps the
  space-separated form (house-correct per `refs/cli.md`) and fails closed by the
  `--tag-only` / `--gates` conflicts plus a hint in `validate_project`'s error. The
  parse-time directory-value check is dropped: it refused valid member names. Repeated-
  only (`num_args = 1`) would also work but forces a matching edit to
  `git-release-guard.sh`, which parses today's form.
- 2026-11-09, author: bug 7 gets true skips, not a shell parser (Alternative 2). Panel
  round 1 M1 pinned the recognizer's one accepted shape (Data Model) and added
  `CustomRoot` and symlink-aware containment.
- 2026-11-09, author, panel round 1 M5: the shakedown suggestion "`--no-tag` should probe
  the gate" is declined. `--no-tag` is the gated flow's own version-commit step; a probe
  there adds noise to every gated release, and `no_tag_skips_gate_probe_even_when_gated`
  pins the skip on purpose.
- 2026-11-09, panel round 1 defers, agreed: `repo_root` git-missing handling (same path
  as today's `is_git_repo`); mid-run rollback (forward-fix, pushed tags stand); Phase 1
  child-process permissions (unquantified).
- 2026-11-09, author: `--gates` UNKNOWN exits 1. No script reads its exit code (hooks
  only pattern-match the command); `/plan:release` already routes non-zero to Branch 5.

## Alternatives Considered

### Alternative 1: Refuse a subdirectory (bug 1)
- **Description:** `<dir> is <prefix> inside <root>; bump runs on a repo root. Run: bump
  <root>`, exit 1, from every entry point.
- **Pros:** fails closed; a subdirectory with its own manifest (mpe-qa-tool's
  `plugin/pyproject.toml`) refuses loudly instead of changing meaning.
- **Cons:** `cd src && bump` stops working where git, cargo and otto all walk up.
- **Why not chosen:** Scott chose the walk-up, 2026-11-09. The notice line covers the
  mpe-qa-tool shape.

### Alternative 2: Parse shell segments in install commands (bug 7)
- **Description:** verify the one cargo-install-of-this-repo segment inside a compound.
- **Pros:** verifies `cargo install --path . && cp ...`.
- **Cons:** a partial shell parser (subshells, `$(...)`, quoting across segments): magic
  that cannot be made predictable (`rules/taste.md`).
- **Why not chosen:** a true skip is honest; the fleet has no compound install today.

### Alternative 3: Fix the bugs as separate targeted commits, no doc
- **Why not chosen:** Scott chose one doc for the whole set; bugs 1 and 7 change
  behavior.

## Technical Considerations

### Dependencies
- No new crates. Internal: Phase 3 builds on Phase 2, Phase 4 on Phase 3
  (`resolve_verb_dir`).

### Performance
- One extra `git rev-parse` per directory; `finish -n` gains a fetch.

### Security
- Bug 1 and bug 2 are wrong-target writes (tags and pushes). Bug 1 closes by acting on
  the right target (the root); bug 2 closes fail-closed.

### Testing Strategy
- Each phase: unit tests on the pure function plus a `tests/release_cli.rs` binary test
  on the fixture shape. Break-the-code check on Phases 4, 5, 8: revert the fix, the new
  test fails.
- `TMPDIR=/media/saidler/4tb/claude-tmp otto ci` in the sandbox.
- After release: the shakedown's fleet `finish -n` sweep with the installed binary,
  plus plain `bump -n` from a fleet subdirectory, `--skip-member`, and `--install` shapes
  (panel round 1, staff).

### Rollout Plan
- One release after Phase 12 (`bump release`, ungated). Then Phase 13, committed and
  pushed in `scottidler/claude`.
- Delete the two merged branches after Phase 3 lands (local and origin).

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| A subdirectory with its own manifest under a versioned root now bumps the root | Low | Low | the notice names the ignored manifest; the generic-root shape (mpe-qa-tool) refuses |
| mpe-qa-tool's `cd plugin && bump`, which works today, now refuses | Med | Low | the refusal says why; mpe-qa-tool is tagged by hand today (its tag messages are not bump's) |
| Shell-syntax scan misreads a quoted or escaped operator | Low | Low | the scan lives in `split_command_words`, which owns quoting and escapes; table tests cover `\;` and `--features "a&&b"` |
| `--gates` exit change breaks a caller | Low | Low | none found; `/plan:release` routes non-zero already |
| Rebased branches drift from their tested state | Low | Med | each phase re-runs the branch's own tests on the rebased tree |

## Open Questions

None.

## References

- `docs/shakedown-v0.4.7.md`
- `docs/plan/2026-11-09-install-verify-clis-only.md` and its implementation notes
- `~/repos/.claude/refs/cli.md` (list-valued flags)
- otto's upward walk: `otto-rs/otto` `src/cli/parser/config.rs`, `src/cli/parser.rs`
