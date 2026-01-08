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
#         Run: git push && git push --tags

git push && git push --tags
```

### 2. Committed but unpushed (auto-amend)

```bash
# You committed changes but forgot to bump
git add .
git commit -m "Add new feature"

# Run bump - amends your commit with version bump
bump -a
# Output: Amended commit and tagged v0.4.3

git push && git push --tags
```

### 3. Committed and pushed (new commit)

```bash
# You committed and pushed, but forgot to bump
git add . && git commit -m "Add feature" && git push

# Run bump - creates a new version bump commit
bump -a
# Output: Committed and tagged v0.4.3

git push && git push --tags
```

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
