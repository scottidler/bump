# bump

CLI tool for bumping semantic versions (Cargo.toml, pyproject.toml, package.json),
creating commits, and tagging releases -- plus two release verbs, `bump release` and
`bump finish`, that drive the whole mechanical release sequence (including pushes,
PR handling, and install) for both ungated and PR-gated repos.

## Installation

```bash
cargo install --path .
```

## The release verbs (recommended)

For releasing a repo, use `bump release` / `bump finish` -- they absorb every mechanical
step (pushes, PR open, the CI wait, install) that the primitives below (bare `bump`,
`--no-tag`, `--tag-only`) otherwise leave to you. Run from inside the repo, bare, no `&&`
and no wrapper:

```bash
bump release [-m|-M] [-n] [--install "<cmd>"|--no-install] [--standalone "<words>"] \
             [--no-ci-gate] [--ci-timeout SECS]
bump finish  [-n] [--install "<cmd>"|--no-install] [--no-ci-gate] [--ci-timeout SECS]
```

`bump release` inspects the repo's git + gate state and either executes the ONE correct
sequence or refuses with the exact next command. Which of the two flows applies is
decided by `bump --gates` (never both):

| Flow | What it does |
|---|---|
| UNGATED | version commit -> push origin `<default>` -> confirm on origin -> wait for green CI -> tag the verified sha -> push tag -> install |
| GATED | version commit rides the feature branch -> push it (no tags follow) -> open a PR if none is open (title/body built from the branch and its commits) -> pause: `merge the PR, then run: bump finish` |

| Situation | `bump release` does |
|---|---|
| Ungated, on default, ahead of origin, no pending version | the UNGATED flow above |
| Ungated, PENDING VERSION (manifest untagged, not below the latest tag) | that version IS the release -- pushed if ahead, never re-bumped past it; an explicit `-m`/`-M` implying a different version refuses naming both |
| Ungated RESUME (pending version, HEAD == origin) | wait for green CI, tag, push tag, install -- never reported as "already released" |
| Ungated, manifest below the latest tag | refuses by name; bump never lowers a version |
| Ungated, not on default / behind / diverged / nothing to release | refuses with the one exact fix (checkout, `git pull --ff-only`, `git pull --rebase`, or `--standalone`) |
| Gated, feature branch, fresh | rides the bump, pushes, opens/ensures the PR, pauses |
| Gated, feature branch, already bumped (version line already in the diff) | skips the re-bump, ensures branch/PR, same pause; a mismatched level refuses naming both |
| Gated, feature branch, PENDING VERSION inherited from the default branch | bumps AGAIN from the manifest version; names the untagged one it is burning unless `bump finish` ships it first |
| Gated, feature branch whose diff vs the default is empty or version-only | refuses, names `--standalone` |
| Gated, feature branch where the title-guard slug != the branch name | refuses before any mutation, prints `git branch -m <slug>` |
| Gated, on default with commits not on origin (stranded) | refuses with the literal rescue commands, never auto-rescued |
| Gated, on default, clean, tagged | refuses "bump rides a feature PR", names `--standalone` |
| Gate unknown, dirty tree, detached HEAD | refuses with the one exact fix |

`--standalone "<words>"` is Scott's words, verbatim: the one legitimate way to ship a
release whose diff is empty or version-only. Re-asking for this order is a violation --
an agent runs this flag only when Scott already gave the words in this session; otherwise
it should STOP and report.

After the PR merges, `bump finish` runs from ANY worktree of the repo (its own checkout of
the default branch, a sibling worktree found via `git worktree list`, or checks it out):

| Situation | `bump finish` does |
|---|---|
| origin/`<default>` carries an untagged version (the merged bump) | fast-forward (if behind) -> wait for green CI on the merged sha -> tag it -> push tag by name -> install |
| origin/`<default>` version == last tag (nothing merged / bump never rode) | refuses "bump rides a feature PR", names `--standalone` on `bump release` as the one door |
| Tag exists on the remote at the merged commit | no-op "already released" -- install still runs, so a re-run after "tag pushed, install failed" installs |
| Tag exists LOCALLY only (RESUME: a prior run died before/during the tag push) | wait for green CI, push the tag, install -- never reported as already released |
| Local default ahead of origin (commits that never landed) | refuses before any pull, with the literal rescue commands |
| Local default diverged from origin | refuses before any pull, names `git pull --rebase origin <default>` |
| Tracked changes in the current OR the resolved worktree | refuses before anything moves |

Full state tables: `bump release --help` / `bump finish --help`, or the design doc
(`docs/design/2026-09-26-one-release-command.md`).

**CI gate:** no tag exists until the pushed (or merged) sha's check runs and legacy commit
status are all green, polled every 15s up to `--ci-timeout` (default 1800s). Red,
truncated, or errored CI refuses with NO tag created; a re-run reuses the same version,
never bumping past it. `--no-ci-gate` skips the wait -- for a human at a terminal who
already knows the repo's CI story, not for an agent to get unstuck.

`--install <cmd>` / `--no-install` control the post-release install step (precedence:
flag override > repo-root `bump.yml`'s `install:` key > `cargo install --path .` iff the
root `Cargo.toml` declares its own `[package]` table > skip). A virtual workspace root
(`[workspace]` with no `[package]`, e.g. a Cargo workspace with no root crate) has no
default install and prints `install: skipped (virtual workspace root; pass --install or
set install: in bump.yml)`. `-n` previews every command the verb would run and executes
nothing.

`gh` calls are authed per-org: a token file, then `GITHUB_PAT_<ORG>`, then
`GITHUB_PAT_WORK` for `tatari-tv` / `GITHUB_PAT_HOME` otherwise, else ambient `gh auth` --
so a work-org PR/CI read never goes out under the wrong account.

## Primitives (for humans / advanced or manual use)

Everything below this point is the set of primitives the two verbs above are built on.
Reach for these directly only when composing your own automation, debugging a release,
or working a repo the verbs don't cover (e.g. multi-directory batch runs).

```bash
bump [OPTIONS] [DIRECTORIES...]
```

### Options

| Flag | Description |
|------|-------------|
| `-M`, `--major` | Bump major version (X.0.0) |
| `-m`, `--minor` | Bump minor version (x.Y.0) |
| (default) | Bump patch version (x.y.Z) |
| `-n`, `--dry-run` | Preview changes without applying |
| `-a`, `--automatic` | Generate automatic commit message |
| `--message <MSG>` | Use custom commit message |
| `-f`, `--force` | Bump even if HEAD already has a tag |
| `--no-tag` | Bump + commit, but create no tag (for PR-gated repos) |
| `--tag-only` | Tag the merged commit (post-merge step for PR-gated repos) |
| `--gates` | Report branch-protection gate status and the recommended flow |
| `--no-verify` | Skip the remote gate probe (treat the repo as ungated) |

> `bump` requires `git` and `gh`. `gh` is used to probe branch-protection gates; if it
> is missing or unauthenticated, `bump` warns and proceeds as if the repo were ungated.

## Workflows

**bump** handles three scenarios:

### 1. Uncommitted changes (standard workflow)

```bash
# Make your changes, leave them unstaged
vim src/main.rs

# Run bump - stages, commits, and tags
bump
# Output: bump: 0.4.2 → 0.4.3
#         Committed and tagged v0.4.3
#         Run: git push origin <branch> && git push origin v0.4.3

git push origin <branch> && git push origin v0.4.3
```

### 2. Committed but unpushed (auto-amend)

```bash
# You committed changes but forgot to bump
git add .
git commit -m "Add new feature"

# Run bump - amends your commit with version bump
bump -a
# Output: Amended commit and tagged v0.4.3

git push origin <branch> && git push origin v0.4.3
```

### 3. Committed and pushed (new commit)

```bash
# You committed and pushed, but forgot to bump
git add . && git commit -m "Add feature" && git push

# Run bump - creates a new version bump commit
bump -a
# Output: Committed and tagged v0.4.3

git push origin <branch> && git push origin v0.4.3
```

## Gated repositories (branch protection / rulesets)

On repos where the default branch is gated - classic branch protection and/or GitHub
rulesets (including org-level required-workflow rulesets) - a commit cannot be pushed
directly to the default branch. It must ride a PR, and the squash-merge rewrites the
commit SHA. Tagging the local commit there produces an **orphaned tag**: the tag points
at a SHA that never lands on the default branch.

`bump` detects this. On a gated repo the default invocation refuses to tag (before any
file or git change) and prints the gated flow instead. Check any repo with:

```bash
bump --gates
# Repo:   tatari-tv/example
# Branch: main
# Gates:  pull_request, workflows (gated)
#
# Gated flow:
#   bump --no-tag [-m|-M]      # version bump rides your branch/PR
#   <push branch, open PR, merge>
#   git checkout main && git pull --ff-only origin main
#   bump --tag-only            # tag the merged commit
#   git push origin vX.Y.Z
```

### Ungated repo (direct push allowed)

```bash
bump [-m|-M]
git push origin <branch>
git push origin vX.Y.Z
```

### Gated repo (PR required)

```bash
# On your feature branch, bump the version without tagging:
bump --no-tag
git push origin my-feature   # open a PR, get it merged

# After the PR merges, on the merged default branch:
git checkout main && git pull --ff-only origin main
bump --tag-only              # verifies HEAD == origin/main, then tags
git push origin vX.Y.Z
```

`--tag-only` refuses unless the working tree is clean, you are on the remote default
branch, and HEAD is **exactly** `origin/<default>` - so it can never tag an unmerged or
stale commit. An existing tag already at HEAD is a no-op; one pointing elsewhere is
refused (resolving that is manual tag surgery, never bump's job).

## Commit Message Behavior

| Situation | Behavior |
|-----------|----------|
| `--message "msg"` provided | Uses provided message |
| `-a` / `--automatic` flag | Generates "Bump version to vX.Y.Z" |
| Only Cargo.toml changes | Auto-generates message |
| Other changes present | Opens editor ($VISUAL → $EDITOR → vim) |

## Multiple Directories

Process multiple Rust projects at once:

```bash
bump ./proj1 ./proj2 ./proj3
```

## Dry Run

Preview what bump would do:

```bash
bump -n
# [dry-run] Would update: Cargo.toml
# [dry-run] Would amend previous commit and tag: v0.4.3
```
