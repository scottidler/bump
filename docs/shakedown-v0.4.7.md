# CLI Shakedown Report: bump v0.4.7

Run 2026-11-09 against the installed `~/.cargo/bin/bump` (`bump v0.4.7`, built from the
v0.4.7 tag `2b4080f`). Focus: the CLI-only install and verify rules from
`docs/plan/2026-11-09-install-verify-clis-only.md`.

Safety: every mutating form ran only in throwaway repos under
`/media/saidler/4tb/claude-tmp/shakedown-bump/`, each with a local bare repo as `origin`.
Real repos under `~/repos/tatari-tv/` and `~/repos/scottidler/` saw only `--gates`,
`-n`, and argument-error runs. A refs and status snapshot of the ten real repos taken
before and after the run is identical. Verify ran end to end with `CARGO_INSTALL_ROOT`
set to a scratch dir inside each fixture; `~/.cargo/bin` was never touched.

## Summary

| Metric | Count |
|--------|-------|
| Commands discovered | 4 forms (`bump`, `bump --gates`, `bump release`, `bump finish`) plus `help`; 13 root flags, 9 `release` flags, 6 `finish` flags |
| Commands tested | all 4 forms, every flag; about 130 `bump` invocations |
| Commands passed | 4 of 4 forms work; every new install and verify behavior matches the design |
| Failures and bugs | 7 bugs, 5 cosmetic, 5 suggestions (below) |
| Commands skipped | real-repo mutations (release, finish, plain bump, `--tag-only`, `--standalone` without `-n`); ungated `bump release` end to end (fails closed on a non-GitHub origin, by design); the `$EDITOR` commit-message path (interactive and hook-blocked); non-git-dir cases (hook-blocked) |
| Pipelines tested | 7 |
| Edge cases tested | 26 |
| Hangs | none. The verify timeout fired at 10.1s as designed |

## Command Results

### Discovery

| Invocation | Exit | Notes |
|---|---|---|
| `bump --version` | 0 | `bump v0.4.7` |
| `bump --help`, `bump -h` | 0 | identical long help; flows block plus the required tools check (`git 2.53.0`, `gh 2.103.3`) |
| `bump release --help` | 0 | full state table; no `[DIR]` positional (see Observations) |
| `bump finish --help` | 0 | full state table |

### `--gates` against real repos (read-only)

| Repo | Verdict |
|---|---|
| scottidler/bump, tatari-tv/sdv, tatari-tv/renew | `none (ungated)` |
| valet, catalog-api, rust-axum-svc, rust-catalog, clyde, marquee, persona, github-actions | `classic_protection, workflows (gated)` |
| fixture with a local bare origin | `UNKNOWN (could not verify: not a GitHub remote)`, exit 0 |

A multi-repo call prints a `[name]` header per repo. A single-repo call prints no header.

### `bump finish -n` across the fleet (the acceptance criteria)

| Repo | `install:` line | `verify:` line |
|---|---|---|
| valet | `skipped (no clap dependency: not a CLI; pass --install or set install: in bump.yml)` | none |
| catalog-api | same | none |
| rust-axum-svc | same | none |
| rust-catalog | same | none |
| clyde | `cargo install --locked --path clyde` | `clyde --version == v0.28.2` |
| sdv | `cargo install --path .` | `sdv --version == v0.5.11` |
| renew | `skipped (library crate, no binary target; ...)` | none |
| marquee | `skipped (virtual workspace root; ...)` | none |
| persona | `skipped (virtual workspace root; ...)` | none |

The three design-doc acceptance criteria that run the binary (1 to 3) pass: four services
skip with the clap reason and print no verify line, clyde is verified against its member,
and sdv and renew are unchanged. Criteria 4 (`cargo test`) and 5 (claude-repo prose) were
not in scope here.

### `bump release -n` on real repos

| Repo | Exit | Result |
|---|---|---|
| bump, sdv | 1 | `nothing to release: nothing ahead of origin/main and the version is already tagged.` plus the `--standalone` door |
| valet | 1 | `this repo is GATED and you are on the default branch 'main'` |
| catalog-api | 1 | `the working tree is dirty` (only an untracked `target/`; see bug 5) |
| persona (on `spec-review-fixes`) | 1 | `manifest 2.0.1 is below the latest tag v2.0.2` |
| sdv `--standalone "shakedown dry-run only"` | 0 | ungated plan: version commit, push, CI wait, tag `v0.5.12`, install, `verify: sdv --version == v0.5.12` |
| valet `--standalone "..."` | 0 | gated plan: `bump-v0-2-3` branch and `gh pr create ... --title "chore: bump v0.2.3"` (see bug 3) |
| sdv `--standalone ""` | 1 | `an empty order is no order` |

### Plain `bump -n` on real repos

| Invocation | Exit | Result |
|---|---|---|
| `bump -n ~/repos/scottidler/bump` | 0 | `0.4.7 -> 0.4.8`, would update Cargo.toml, commit and tag |
| `bump -n -m sdv clyde marquee` | 1 | sdv previews `0.6.0`; clyde and marquee refuse as gated; `Completed: 1 succeeded, 2 failed` |
| `bump -n --no-tag clyde` | 0 | `skipping claude-pricing (independent version 2.0.0)`, would commit `0.28.3` with no tag |
| `bump -n --tag-only clyde` | 0 | `v0.28.2 is already tagged at HEAD on origin. Nothing to do.` |

### Install and verify, end to end in fixtures

The fixtures: `cli` (clap, `version = concat!("v", CARGO_PKG_VERSION)`), `svc` (axum, no
clap, `main` loops forever), `lib` (clap but no bin), `ws` (virtual root, member `tool`,
`bump.yml` `install: cargo install --locked --path tool`), `py` (pyproject only), `gen`
(no manifest).

Real `cargo install` into a scratch `CARGO_INSTALL_ROOT`:

```
$ bump finish --no-ci-gate            # cli, version untagged on origin
CI gate: SKIPPED (--no-ci-gate); v0.1.1 is tagged without waiting for CI on fb3e4aa...
released v0.1.1 on main
install: cargo install --path .
...
verify: .../cli/target/root/bin/shakecli --version -> shakecli v0.1.1
EXIT 0        (origin tag v0.1.1: type `tag`, points at fb3e4aa)

$ bump finish --no-ci-gate            # ws, already released row
already released v0.4.0
install: cargo install --locked --path tool
...
verify: .../ws/target/root/bin/shaketool --version -> shaketool v0.4.0
EXIT 0
```

A fake `cargo` on PATH wrote a scripted `shakecli` into the install root. Every failure
mode came out as the design specifies:

| Fake `--version` behavior | Exit | Message (abridged) |
|---|---|---|
| prints `shakecli v0.1.0` (stale) | 1 | `printed "shakecli v0.1.0", expected v0.1.1.` / `verify ran <root>/bin/shakecli (from the install root)` / no hint / closing re-run line |
| prints `v0.1.1-3-gabc1234` | 1 | adds `The binary was built past the tag (a -N-g<sha> build): HEAD is not the tagged commit.` |
| prints `0.1.1` (no v) | 1 | adds `The binary reports CARGO_PKG_VERSION (no v), which the build-version contract never prints.` |
| prints `v0.1.10` (substring trap) | 1 | mismatch, no hint: whole-token match holds |
| exits 3 with stderr | 1 | `exited exit status: 3.` then `stdout: partial out` / `stderr: boom: missing SIGNING_KEY` |
| sleeps 60s (a server) | 1 | `did not exit within 10s; a CLI answers --version at once.` plus `stdout: starting server`; wall clock 10.1s |
| backgrounds a 30s grandchild, prints `v0.1.1` | 0 | passed in 0.2s; the orphan did not block |
| not in the install root, older copy on PATH | 1 | `verify ran shakecli (from PATH)` |
| not anywhere | 1 | `could not run `shakecli --version`: No such file or directory (os error 2)` plus the closing line |
| install command exits 101 | 1 | `install command failed: cargo install --path .` (see suggestion 3) |

Each verify failure ends with: `The tag is pushed and stands. A source fix is a new release:
commit it and run `bump release`. To only reinstall, re-run `bump finish` (repeat any
--install override).`

### `--install` parsing (dry-run in the `ws` and `svc` fixtures)

| `--install` value | `verify:` line |
|---|---|
| `cargo install --locked --path "tool"` | `shaketool --version == v0.4.0` |
| `cargo +nightly install --path=tool` | `shaketool --version == v0.4.0` |
| `cargo install --features "a b" --path tool` | `shaketool --version == v0.4.0` |
| `cargo install --path 'my tool'` and `--path my\ tool` | `spacetool --version == v0.4.0` |
| `cargo install` (virtual root) | `skipped (no binary name in .../ws/Cargo.toml)` |
| `make install` | `skipped (install command is not cargo install)` |
| `cargo install ripgrep`, `--git URL`, `--registry foo --path tool` | `skipped (install command does not install this repo)` |
| `cargo install --path .` on svc | `skipped (no clap dependency in .../svc/Cargo.toml)` |
| `cargo install --path tool && echo done` | `skipped (install command does not install this repo)` (bug 7) |
| `RUSTFLAGS=-g cargo install --path tool` | `skipped (install command is not cargo install)` (bug 7) |
| `cargo install --path ../cli` | `shakecli --version == v0.4.0` (bug 7) |
| `cargo install --path nonexistent` | `skipped (no binary name in .../nonexistent/Cargo.toml)` |
| `""` | `install: ` (empty) then `verify: skipped (install command is not cargo install)` (bug 6) |

### Primitives in fixtures (mutating, throwaway only)

| Invocation | Result |
|---|---|
| `bump --no-verify --no-tag` on an unpushed commit | `Amended commit with version bump to 0.1.1 (no tag)`; amends Cargo.toml and Cargo.lock into it |
| `bump --no-verify --tag-only` with HEAD ahead of origin | refuses: `HEAD is ahead of origin/main; the commit to tag is not merged/pushed yet.` |
| `bump --no-verify --tag-only` after the push | `Tagged v0.2.1 on merged main (4890d9a85dd5)`; annotated; a re-run says `already tagged at HEAD locally` |
| `bump finish --no-ci-gate` with a local-only tag | `resumed release: pushed v0.2.1 on main` |
| `bump finish --ci-timeout 20` on a local origin | `CI gate: no GitHub remote, nothing to wait on`, then tags |
| `bump --no-verify --message "..."` | new commit with that subject; on the amend path it replaces the original subject |
| `bump --no-verify -a` | `Bump version to v0.3.3` |
| `bump --no-verify` on `py` | `0.5.0 -> 0.5.1`, pyproject.toml updated, annotated tag |
| `bump --no-verify -m` on `gen` | `unknown -> 0.7.0`, tag only (cosmetic 1) |
| `bump --no-verify --force` on a tagged HEAD | second tag on the same commit |

## Output Format Matrix

bump has no output-format flags (`--json`, `--format`, `--csv`): every surface is plain
text. Errors go to stderr (`bump release -n | wc -c` prints `0` with the error still on the
terminal), so `grep`/`awk` over stdout work.

| Surface | Text | JSON | Notes |
|---|---|---|---|
| `--gates` | yes | none | aligned `Repo:`/`Branch:`/`Gates:` block; a `[name]` header only with several repos |
| `release -n` / `finish -n` | yes | none | one `[dry-run]` line per step; easy to grep |
| plain `bump -n` | yes | none | |
| errors | yes, stderr | none | clap errors exit 2, bump refusals exit 1 |

## Failures & Bugs

1. **Bug (high): a subdirectory is released as a generic repo and tagged out of step with
   the manifest.**
   - Command: `cd <rust-repo>/src && bump` (same with `bump <rust-repo>/src`).
   - Actual: `bump: unknown -> 0.1.2` / `Committed and tagged v0.1.2` /
     `Run: git push origin main && git push origin v0.1.2`. No commit is made, Cargo.toml
     stays `0.1.1`, and an annotated `v0.1.2` lands on HEAD.
   - Expected: resolve the repo root (`git rev-parse --show-toplevel`) or refuse because the
     dir is not a repo root. `[DIRECTORIES]` is documented as "Paths to git repository
     roots", yet nothing enforces it.
   - Related: `bump finish -n` from the same subdir refuses with the wrong reason,
     `this repo has no version-bearing manifest (generic)`.
2. **Bug (medium): the variadic `--skip-member` swallows the directory positional.**
   - Command: `bump -n --skip-member claude-pricing ~/repos/tatari-tv/clyde`.
   - Actual: exit 1, `--skip-member is only meaningful for a Cargo workspace, but this is a
     Generic project.` The clyde path was parsed as a second member name and the cwd was
     bumped.
   - Expected: the path is the positional. `bump -n --no-tag ~/repos/tatari-tv/clyde
     --skip-member claude-pricing` works. Fix: `num_args = 1` with `ArgAction::Append`, so
     the flag is repeated rather than space-separated.
3. **Bug (medium, policy drift): gated `bump release` opens the PR itself with a
   conventional-commit title.**
   - Command: `bump release -n --standalone "..."` in valet.
   - Actual: `gh pr create --head bump-v0-2-3 --base main --title "chore: bump v0.2.3" ...`.
     `pr_title` (`src/release/pr.rs`) always builds `<type>(<scope>): <words>`, falling
     back to `chore`.
   - Expected under current policy: no `type:` prefix on any PR title (general.md, enforced
     by `pr-title-guard.sh`). The routed git rule also says `bump release` never opens the
     PR and hands off to `/pr:create`. That change exists only on the unmerged local branch
     `leave-pr-to-pr-create` (`3208457`, "Stop bump release from opening PRs; hand the PR
     to /pr:create"), so v0.4.7 and the rule disagree. The guard hook cannot catch it,
     since bump calls `gh` itself.
4. **Bug (low): plain `bump -n` skips the HEAD-already-tagged guard.**
   - Command: `bump -n --no-verify` in a repo whose HEAD carries `v0.7.0`.
   - Actual: `[dry-run] Would commit and tag: v0.7.1`, exit 0. The real run refuses:
     `HEAD already has a tag. Make changes first, then run bump. Use --force to override.`
   - Expected: the dry-run reports the same refusal.
5. **Bug (low): `bump release`'s dirty-tree refusal gives advice that cannot clear an
   untracked-only tree.**
   - Command: `bump release -n` with only an untracked file (real case: catalog-api, whose
     `target/` is not gitignored).
   - Actual: `the working tree is dirty ... Commit or stash your changes first`. `git stash`
     answers `No local changes to save`, and the refusal stands.
   - Expected: list the offending paths and name the untracked case (`git stash -u`, add to
     `.gitignore`, or remove). Counting untracked files is correct, because release runs
     `git add -A`.
6. **Bug (low): `--install ""` is accepted.**
   - Actual: `[dry-run] install: ` with an empty command; a real run would `sh -c ""` and
     report a successful install.
   - Expected: refuse an empty command, as `--standalone ""` already does.
7. **Bug (low): compound install commands get the wrong verify verdict.**
   - `cargo install --path tool && echo done`: `does not install this repo`, but it does.
   - `RUSTFLAGS=-g cargo install --path tool`: `install command is not cargo install`, but it
     is.
   - `cargo install --path ../cli`: verifies another repo's binary against this repo's tag.
   - Verify is skipped, or misdirected, with a reason that is wrong. Low impact: no fleet
     `bump.yml` uses these shapes today.

### Cosmetic

1. Plain bump on a generic repo prints `unknown -> 0.6.1` (the last tag is `v0.6.0`) and
   `Committed and tagged` when no commit was made. The dry-run says `Would create new commit
   and tag`.
2. `--gates` over several repos ends with `All done! Don't forget to push your changes.`,
   which does not fit a read-only report. `Branch: main` prints the default branch even when
   the checkout is on another branch (catalog-api on `extraenv-rust-log`); the label should
   be `Default:`.
3. The verify non-zero message reads `exited exit status: 3.`
4. The dry-run says `Would update: Cargo.toml`, but the real run also rewrites
   `Cargo.lock`. Wording also varies: `Would create new commit and tag` (one repo) against
   `Would commit and tag` (other paths).
5. `Logs are written to:` prints `~/.local/...` in root help and `/home/saidler/.local/...`
   in subcommand help.

### Suggestions

1. Make `finish -n` state-aware: valet is already released at `v0.2.2`, yet the dry-run
   lists the tag and push steps, qualified only by a parenthetical.
2. Give the non-Cargo skip a reason: `py` prints a bare `install: skipped`, unlike the
   three Cargo reasons.
3. Add the exit status and the same "tag is pushed and stands ... re-run `bump finish`"
   closing line to install failures (`install command failed: cargo install --path .`).
   An install failure also comes after the tag.
4. Print the resolved path in the PATH fact line (`verify ran shakecli (from PATH)`), since
   that is the line a stale-copy diagnosis needs.
5. Exit non-zero from `--gates` on an `UNKNOWN` verdict. Also warn on `--gates
   --no-verify` (silently ignored) and on `--no-tag` against a gated default branch
   (previewed with no warning on clyde `main`).

## Pipeline Recipes

```bash
# Acceptance: services never install (prints four clap-skip lines, no verify lines)
for r in valet catalog-api rust-axum-svc rust-catalog; do
  sh -c "cd ~/repos/tatari-tv/$r && bump finish -n" | grep -E '^\[dry-run\] (install|verify)'
done

# Which CLI gets verified, and against what
sh -c 'cd ~/repos/tatari-tv/sdv && bump finish -n' | grep -E '^\[dry-run\] (install|verify)'
# [dry-run] install: cargo install --path .
# [dry-run] verify: sdv --version == v0.5.11

# Gate verdicts for several repos, one line each
bump --gates ~/repos/tatari-tv/valet ~/repos/tatari-tv/sdv ~/repos/tatari-tv/clyde \
  | awk '/^\[/{r=$0} /^Gates:/{sub(/^Gates: +/,""); print r, $0}'
# [valet] classic_protection, workflows (gated)
# [sdv] none (ungated)
# [clyde] classic_protection, workflows (gated)

# Branch on the gate
bump --gates ~/repos/tatari-tv/valet | grep -q '(gated)' && echo "gated: release on a feature branch"

# Bare version string
bump --version | awk '{print $2}'      # v0.4.7

# Errors are on stderr: stdout stays empty on a refusal
sh -c 'cd ~/repos/tatari-tv/sdv && bump release -n' | wc -c   # 0
```

`| grep verify` (the design doc's clyde criterion) also matches the
`(re-verify before the push)` line. Anchor on `'^\[dry-run\] verify:'`.

## Edge Cases

| Case | Result | Graceful? |
|---|---|---|
| `-M -m`, `--no-tag --tag-only`, `--install X --no-install` | clap conflict, exit 2 | yes |
| `--ci-timeout abc` / `--ci-timeout -5` | `invalid digit` / `unexpected argument '-5'`, exit 2 | yes |
| `--standalone ""` | refused, exit 1 | yes |
| `--install ""` | accepted | no (bug 6) |
| `--skip-member nope` (dir first) | `named member(s) that are not independently versioned: nope` | yes |
| `--skip-member X <dir>` | dir swallowed | no (bug 2) |
| repo subdirectory | tags a generic version | no (bug 1) |
| dirty tracked tree: `release -n` / `finish -n` / `finish` | refused, exit 1, before anything moves | yes |
| dirty tracked tree: plain `bump -n` | previews commit and tag without naming the swept files | by design (stages all), but see the hook note below |
| untracked-only tree: `release -n` | refused with advice that does not work | partly (bug 5) |
| detached HEAD: `release -n` | `HEAD is detached; bump release runs on the default branch.` | yes |
| detached HEAD: `finish -n` | plans `git checkout main` | yes (documented) |
| detached HEAD: plain `bump -n` | previews commit and tag, no warning | weak |
| HEAD already tagged: plain `bump` / `bump -n` | real refuses; dry-run does not | no (bug 4) |
| no manifest (`gen`): `finish -n` | `Gated generic repos are unsupported`, exit 1 | yes (the wording assumes gated while the gate is UNKNOWN) |
| non-GitHub origin: `release -n` | `gate status is UNKNOWN ... refuses to guess (fail closed)` | yes |
| non-GitHub origin: plain `bump -n` | warns `proceeding as ungated` | yes (documented) |
| no `origin/HEAD`: `finish -n` | `could not determine the remote default branch ... Try: git remote set-head origin -a` | yes |
| manifest below the latest tag (persona branch) | refused by name | yes |
| non-git dir, nonexistent dir | not run: the local `rails` PreToolUse hook rejects the call before bump starts | n/a |

## Release Validation

- Tag: `git cat-file -t v0.4.7` prints `tag` (annotated), message `Release v0.4.7`, and it
  dereferences to `2b4080f` ("Bump version to v0.4.7"). It is on origin with the same peeled
  sha. Cargo.toml at `2b4080f` is `0.4.7`. Local main is one commit past it (`d4b08a4`,
  marking the design doc implemented).
- Release: https://github.com/scottidler/bump/releases/tag/v0.4.7, published
  2026-11-10T03:18:28Z by github-actions[bot], not a draft, not a prerelease, marked Latest.
- Assets: `linux-amd64`, `linux-arm64`, `macos-arm64`, `macos-x86_64`, each a `.tar.gz` with
  a `.sha256`. All four expected targets are present.
- Binary test: downloaded `bump-v0.4.7-linux-amd64.tar.gz`; `sha256sum -c` OK
  (`33b81303...dbeb`); the archive holds a single `bump` (ELF x86-64); `bump --version`
  prints `bump v0.4.7`, identical to the installed binary.

## Observations

- marquee and persona are clap CLIs with virtual workspace roots and no `bump.yml`, so the
  default install skips them (`virtual workspace root`). The design doc's Alternative 1
  calls them "clap CLIs bump must install". If they should be installed on release, each
  needs `install: cargo install --path cli` in `bump.yml`; bump's behavior is as
  documented.
- `finish`'s CI gate treats a non-GitHub remote as `nothing to wait on` and tags, while
  `release` fails closed on an UNKNOWN gate. That is defensible, since a local remote has
  no CI, but the asymmetry is not documented.
- `release` and `finish` take no `[DIR]` positional in v0.4.7. That change (`eba0468`)
  sits on the unmerged `release-verbs-take-dir` branch; the routed git rule already
  anticipates this.
- `--message` on the auto-amend path replaces the amended commit's original subject. That
  is intentional (`amend_commit_with_message`), but the help line does not say so.
- Harness notes, not bump issues: the `rails` PreToolUse hook misreads `2>&1` and
  `>/dev/null` as directories, treats `help` in `bump help release` as a repo path, reads
  loop variables (`$r`) as paths, checks the cwd rather than a plain-bump positional dir
  for its dirty-tree guard, and rejects non-git-dir edge cases outright. These cases were
  run through `cd <dir> && bump ...` or `sh -c`, or skipped.
