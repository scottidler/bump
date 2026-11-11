# CLI Shakedown Report: bump v0.4.10

Run 2026-11-11 against the installed `~/.cargo/bin/bump` (`bump v0.4.10`, built from the
v0.4.10 tag `3c77ed5`). Focus: re-check every bug in `docs/shakedown-v0.4.7.md` after the
v0.4.8 shakedown fixes, and the two v0.4.9 changes (`release -n` previews a `ci: none`
skip; test git calls run against a trap global config).

Safety: every mutating form ran only in throwaway repos under
`/media/saidler/4tb/claude-tmp/shakedown-bump-049/`, each with a local bare repo as
`origin`. Real repos (scottidler/bump, tatari-tv/valet, clyde, whitespace) saw only
`--gates` and `-n` runs.

## Summary

| Metric | Count |
|--------|-------|
| Commands discovered | 4 forms (`bump`, `bump --gates`, `bump release`, `bump finish`) plus `help` |
| Commands tested | all 4 forms; about 45 `bump` invocations |
| Commands passed | 4 of 4 forms |
| v0.4.7 bugs re-checked | 7: 6 fixed, 1 fixed by design (fails closed with a pointed error) |
| New bugs found | 1 high (v0.4.9 published no release binaries), fixed in v0.4.10 |
| Cosmetic | 2 (below) |
| Commands skipped | real-repo mutations; `$EDITOR` commit-message path (interactive) |
| Pipelines tested | 4 |
| Edge cases tested | 9 |
| Hangs | none |

## Command Results

### Discovery

| Invocation | Exit | Notes |
|---|---|---|
| `bump --version` | 0 | `bump v0.4.10` |
| `bump --help` | 0 | flows block, required tools (`git 2.53.0`, `gh 2.103.3`), log path |
| `bump release --help` | 0 | `[DIR]` positional present |
| `bump finish --help` | 0 | `[DIR]` positional present |

### `ci: none` preview (the v0.4.9 fix)

| Fixture | `bump release -n` CI line | Real run |
|---|---|---|
| `cinone` (`bump.yml`: `ci: none`, committed) | `[dry-run] CI gate: skipped, bump.yml at <HEAD> declares \`ci: none\`` | `CI gate: skipped, bump.yml at <version sha> declares \`ci: none\``, tagged `v0.1.1`, exit 0 |
| `plain` (no `bump.yml`) | `[dry-run] wait for green CI on the sha (... every 15s, up to 1800s)` | not run (no GitHub remote) |

The preview and the run now agree. v0.4.8 printed the CI wait in both fixtures.

### v0.4.7 bugs, re-checked

| # | v0.4.7 bug | v0.4.10 result |
|---|---|---|
| 1 | subdir released as a generic repo | fixed: `.../plain/src is src inside .../plain; running on .../plain`, then `0.1.0 -> 0.1.1` |
| 2 | `--skip-member` swallows the directory | by design: still variadic, but fails closed: `If foo, <dir> is a directory, put directories before --skip-member.` exit 1 (shakedown-fixes doc, Resolved Decisions) |
| 3 | gated `release` opens a PR with a `chore:` title | fixed: `bump opens no PR`; prints the `/pr:create` hand-off and `Release: rides this PR (v0.1.1)` |
| 4 | `-n` skips the HEAD-already-tagged guard | fixed: `-n` refuses with `HEAD already has a tag ...`, exit 1 |
| 5 | untracked-only dirty tree, unusable advice | fixed: lists `?? stray.txt`, says `git stash -u`, remove, or gitignore |
| 6 | `--install ""` accepted | fixed: `install command is empty; pass --no-install to skip the install`, exit 1 |
| 7 | compound install commands, wrong verify verdict | fixed: each skip reason is now true (`uses shell syntax`, `no clap dependency`, `no binary name in .../../cli/Cargo.toml`) |

### v0.4.7 cosmetic items and suggestions, re-checked

| Item | v0.4.10 |
|---|---|
| generic repo `unknown -> 0.6.1` | fixed: `v0.6.0 -> 0.6.1`, `[dry-run] Would tag: v0.6.1` |
| `--gates` footer and `Branch:` label | fixed: no footer; label is `Default:` |
| verify `exited exit status: 3` | install failure reads `install command failed: sh -c 'exit 3' (exit status: 3)` |
| log path differs between helps | fixed: both print `/home/saidler/.local/share/bump/logs/bump.log` |
| `finish -n` not state-aware | fixed: `already released v0.2.2; would run: nothing` (valet), `would run: install, verify` (clyde, whitespace) |
| install failure has no recovery line | fixed: `The tag is pushed and stands. ... re-run \`bump finish <dir>\`` |
| `--gates` exit 0 on UNKNOWN | fixed: exit 1, `gate verdict UNKNOWN: not a GitHub remote` |
| `--gates --no-verify` silently ignored | fixed: clap conflict, exit 2 |

### Real repos (read-only)

| Invocation | Result |
|---|---|
| `bump --gates ~/repos/scottidler/bump ~/repos/tatari-tv/whitespace` | both `none (ungated)`, exit 0. whitespace confirmed by hand: classic protection 404, rulesets `[]` |
| `bump release -n ~/repos/scottidler/bump` | `nothing to release: nothing ahead of origin/main and the version is already tagged`, exit 1 |
| `bump finish -n ~/repos/tatari-tv/valet` | `already released v0.2.2; would run: nothing`, install skipped (no clap) |
| `bump finish -n ~/repos/tatari-tv/clyde` | `already released v0.28.2; would run: install, verify`; `verify: clyde --version == v0.28.2` |
| `bump finish -n ~/repos/tatari-tv/whitespace` | `already released v0.2.0; would run: install, verify` |

## Output Format Matrix

bump has no `--json` or `--csv`. Every form prints plain lines; `-n` lines carry a
`[dry-run]` prefix, which makes them easy to filter (see Pipeline Recipes).

## Failures & Bugs

1. **Bug (high, fixed in v0.4.10): v0.4.9 published no release binaries.**
   - Cause: v0.4.9 added `.cargo/config.toml` `[env]` pointing `GIT_CONFIG_GLOBAL` at
     `tests/fixtures/gitconfig`. Cargo's `[env]` also reaches `build.rs`, whose
     `build-version` step runs `git rev-parse`. The release workflow builds in a container
     where the checkout belongs to another uid; `actions/checkout` trusts it through the
     global config the override replaced. Both Linux builds died with
     `fatal: detected dubious ownership in repository at '/__w/bump/bump'`. The `ci`
     workflow does not run in a container, so it stayed green.
   - Fix: `db25747` adds `[safe] directory = *` to the trap config. Proved in an
     `alpine:3` container as root against the repo: the old config exits 128 (dubious
     ownership), the new one exits 0.
   - Result: v0.4.10's release run is green with all 8 assets. The `v0.4.9` tag stands
     with no GitHub release.

### Cosmetic

1. The `ci: none` preview names HEAD's sha (`bump.yml at d57093cf`); the real run names the
   version commit's sha. Both are correct (the version commit carries HEAD's `bump.yml`),
   but the two lines show different shas.
2. Path errors differ in case: `release` says `not a git repository: /var/tmp`, plain
   `bump` says `Not a git repository: ...`. A mistyped subcommand (`bump nope-subcommand`)
   is read as a directory and reports `Not a git repository`.

## Pipeline Recipes

```bash
bump --version | awk '{print $2}'
# v0.4.10

bump -n --no-verify <generic-repo> | sed -n 's/^bump: .* -> //p'
# 0.6.1   (the next version)

bump --gates ~/repos/scottidler/bump | awk -F': *' '/^Gates/{print $2}'
# none (ungated)

bump finish -n ~/repos/tatari-tv/clyde | sed -n 's/^\[dry-run\] verify: //p'
# clyde --version == v0.28.2
```

## Edge Cases

| Invocation | Result |
|---|---|
| `bump release -n /nonexistent` | `not a git repository: /nonexistent` |
| `bump release -n /var/tmp` | `not a git repository: /var/tmp` |
| `bump -M -m -n` | clap conflict, exit 2 |
| `bump --tag-only --no-tag -n` | clap conflict, exit 2 |
| `bump release -n --ci-timeout abc` | `invalid digit found in string`, exit 2 |
| `bump release -n --standalone ''` | `an empty order is no order`, exit 1 |
| `bump finish -n` with a local-only commit on main | refuses, prints the `git branch stranded-<sha>` rescue |
| `bump release` with a failing `--install` | tag pushed, `exit status: 3`, recovery line, exit 1 |
| `bump nope-subcommand` | read as a directory: `Not a git repository` |

## Release Validation

- Tag: `v0.4.10` exists, annotated (`git cat-file -t` prints `tag`), points at `3c77ed5`
  = `origin/main`.
- Release: published, not a draft.
- Assets: linux-amd64, linux-arm64, macos-arm64, macos-x86_64, each `.tar.gz` plus
  `.sha256`.
- Binary test: `bump-v0.4.10-linux-amd64.tar.gz` passes `sha256sum -c`, and the extracted
  binary prints `bump v0.4.10`, matching the installed one.
- `v0.4.9`: tag exists, no GitHub release (bug 1).

## Observations

- `--gates` and the session's git hook disagree on `tatari-tv/whitespace`: bump says
  ungated, which matches GitHub (no protection, no rulesets). The hook's "tatari-tv main
  is PR-gated" is a policy, not a gate bump can see.
- The test suite fails 4 tests when `TMPDIR` is a symlink (`/tmp/claude-1000` here):
  `worktree_for_branch_finds_sibling_worktree_and_none_otherwise` and three
  `finish_*_refuses_*` tests compare un-canonicalized paths. `TMPDIR=/var/tmp` passes all.
  Present before v0.4.9.
