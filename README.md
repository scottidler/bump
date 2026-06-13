# bump

Rust CLI tool for bumping semantic versions in Cargo.toml, creating commits, and tagging releases.

## Installation

```bash
cargo install --path .
```

## Usage

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
