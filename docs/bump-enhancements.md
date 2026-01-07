# bump enhancements

This document describes the enhanced features added to the `bump` CLI tool.

## new features

### 1. auto-amend for committed files

When you've already committed your changes but haven't run `bump` yet, the tool now handles this gracefully:

**scenario: unpushed commit**
If HEAD has no tag and hasn't been pushed to the remote:
- Updates Cargo.toml with the new version
- Amends the previous commit to include the version bump
- Creates an annotated tag

```bash
# You committed changes but forgot to bump
git add .
git commit -m "Add new feature"

# Now run bump - it will amend your commit
bump
# Output: Amended commit and tagged v0.1.6
```

**scenario: pushed commit**
If HEAD has already been pushed to the remote:
- Updates Cargo.toml with the new version
- Creates a new commit (doesn't amend, since that would require force push)
- Creates an annotated tag

```bash
# You committed and pushed, but forgot to bump
git add .
git commit -m "Add new feature"
git push

# Now run bump - it creates a new bump commit
bump
# Output: Committed and tagged v0.1.6
```

### 2. commit message flags

Two new flags control commit message behavior:

**`--message <MSG>`** (long-only, no short flag)
Provide a custom commit message directly:

```bash
bump --message "Release with new authentication system"
```

**`-a` / `--automatic`**
Generate an automatic commit message ("Bump version to vX.Y.Z"):

```bash
bump -a
bump --automatic
```

These flags are mutually exclusive. If neither is provided, the tool uses smart defaults:
- Version-only changes (just Cargo.toml/Cargo.lock): auto-generates message
- Other changes: opens your editor

### 3. editor-based commit message input

When you have staged changes beyond just Cargo.toml, the tool now opens your editor (like `git commit` does) instead of prompting on stdin.

**editor selection order:**
1. `$VISUAL` environment variable
2. `$EDITOR` environment variable
3. `vim` (fallback)

**template format:**
```
<type your message here>

# Enter commit message above.
# Lines starting with '#' will be ignored.
#
# Staged changes:
#   src/main.rs
#   src/lib.rs
#   Cargo.toml
#
# An empty message aborts the commit.
```

This fixes issues with backspace and other special characters that didn't work correctly with the previous stdin-based input.

## cli reference

```
bump [OPTIONS] [DIRECTORIES...]

Options:
  -M, --major       Bump major version (X.0.0)
  -m, --minor       Bump minor version (x.Y.0)
  -n, --dry-run     Preview changes without applying
      --message     Commit message to use
  -a, --automatic   Generate automatic commit message
  -h, --help        Print help
  -V, --version     Print version

Arguments:
  [DIRECTORIES...]  Paths to git repository roots
```

## workflow examples

### standard workflow (uncommitted changes)
```bash
# Make changes
vim src/main.rs

# Run bump - stages, commits, and tags
bump
```

### agent/ci workflow (already committed)
```bash
# Agent commits changes
git add .
git commit -m "Implement feature X"

# Run bump - amends commit and tags (if unpushed)
bump -a
```

### explicit message workflow
```bash
# Make changes and run with custom message
bump --message "v1.0.0 - Production release"
```

### dry-run to preview
```bash
bump -n
# [dry-run] Would update: Cargo.toml
# [dry-run] Would amend previous commit and tag: v0.1.6
```
