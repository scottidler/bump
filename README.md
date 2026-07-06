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
step (pushes, PR open, install) that the primitives below (bare `bump`, `--no-tag`,
`--tag-only`) otherwise leave to you. Run from inside the repo:

```bash
bump release [-m|-M] [-n] [--install "<cmd>"|--no-install]
bump finish  [-n] [--install "<cmd>"|--no-install]
```

`bump release` inspects the repo's git + gate state and either executes the ONE correct
sequence or refuses with the exact next command:

| Situation | `bump release` does |
|---|---|
| Ungated, on default, ahead of origin | version commit -> push branch -> confirm on origin -> tag -> push tag -> install |
| Ungated, not on default / behind origin / nothing to release | refuses with the exact fix |
| Ungated RESUME (a prior run died between branch push and tag push) | tags (if needed) and pushes the tag, without re-bumping or falsely claiming "already released" |
| Gated, on a feature branch, fresh | rides the bump on the branch (`--no-tag` internally), pushes it, opens a PR if none is open, then PAUSES: `merge the PR, then run: bump finish` |
| Gated, on a feature branch, already bumped | skips the re-bump, ensures the branch/PR, same pause |
| Gated, on default with stranded commits | refuses with the literal rescue commands (never auto-rescues) |

After the PR merges, `bump finish` fast-forwards to the merged tip and tags it:

| Situation | `bump finish` does |
|---|---|
| origin/`<default>` carries an untagged version (the merged bump) | checkout -> `pull --ff-only` -> tag the merged commit -> push tag -> install |
| Nothing merged / bump never rode | refuses: "bump rides a feature PR -- run bump release on a branch" |
| Already tagged at the merged commit | no-op: "already released" |
| A tag exists locally only (a prior run died mid-push) | resumes: pushes the tag, never reports it as already released |

Full state tables: `bump release --help` / `bump finish --help`, or the design doc
(`docs/design/2026-07-06-release-verbs-and-language-adapters.md`).

`--install <cmd>` / `--no-install` control the post-release install step (precedence:
flag override > repo-root `bump.yml`'s `install:` key > `cargo install --path .` iff a
Cargo.toml is present > skip). `-n` previews every command the verb would run and
executes nothing.

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
