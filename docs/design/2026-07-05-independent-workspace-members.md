# Support workspaces with an independently-versioned member

**Date:** 2026-07-05
**Status:** Implemented

## Problem

`bump` aborts on any Cargo workspace where one member does not inherit
`version.workspace = true`. It scans the members up front and, if it finds a member
carrying its own literal `version = "..."`, bails with:

```
Error: Workspace members have independent versions (not using version.workspace = true):
  - claude-pricing (pricing): 2.0.0
bump only supports workspaces with a unified version in [workspace.package].
```

This is a conservative guard, not a correctness limit. `bump` only ever edits
`[workspace.package].version`; it does not touch a member that pins its own version.
So it *could* bump the workspace version and leave the pinned member alone, but instead
it refuses to run at all.

## Concrete case: tatari-tv/clyde

clyde is a workspace whose members inherit `version.workspace = true`, except
`claude-pricing`, which pins `version = "2.0.0"` on purpose: its major is contractually
locked to the pricing-feed `schema_version`, and dropping it to the 0.x workspace line
would make fetched feeds reject the library as too old. That pin is intentional and must
stay.

Result: plain `bump` and `bump --no-tag` can never run on clyde -- both flow through the
up-front independent-version guard and abort. (`bump --tag-only` is not blocked: it returns
before the guard and reads only `[workspace.package].version`, so the pinned member is
invisible to it. But it only tags an *already-merged* bump commit, and on a gated repo the
only way to produce that commit is `bump --no-tag` on the feature branch -- which aborts. So
the release is blocked at the `--no-tag` step regardless.) The release has to be done by
hand, which is how a release-branch anti-pattern crept in (see `~/HALL-OF-SHAME.md`,
2026-07-05).

## Options

1. **Change claude-pricing** so it is not independently versioned. Rejected: the 2.0.0
   pin is a deliberate contract with the pricing feed; unifying it breaks feed
   compatibility.
2. **Teach `bump` to tolerate an independently-versioned member (chosen).** The guard
   becomes opt-out instead of fatal.

## Proposed design

- Add one flag: `--skip-member <name>` (repeatable, space-separated per the CLI rule).
  Each value names a workspace member whose literal `version =` is left untouched. There
  is deliberately **no** blanket `--allow-independent`: a blanket flag turns the guard that
  caught this problem class into "accept every accidental pin in the workspace." The real
  need is one named, contractually-pinned member, and a named exception is auditable where a
  blanket allow is not.
- `--skip-member` matches on the **package name** (`claude-pricing`), not the member path
  (`pricing`). Diagnostics still print both, matching the guard's existing
  `name (path): version` format (`src/main.rs:112`).
- The guard stays strict; it fails **closed** on any mismatch, always before any mutation:
  - An independent member that is **not** named by `--skip-member` still aborts (the guard's
    whole reason for existing).
  - A `--skip-member <name>` that matches **no independent member** (wrong name, or a member
    that already inherits `version.workspace`) aborts with a clear error, so a stale skip flag
    can't rot silently in CI.
- With every independent member accounted for by a `--skip-member`, the run proceeds: bump
  `[workspace.package].version`, leave each named member's literal `version =` untouched.
- Print each skip to the **terminal** (stdout), not via `info!`. `bump` routes `log`/
  `env_logger` to `~/.local/share/bump/logs/bump.log` (`src/main.rs:142`), so an `info!` line
  is invisible to the operator; the "never silent" guarantee requires `println!`:

  ```
  skipping claude-pricing (independent version 2.0.0)
  ```

- Update the fatal guard message in `validate_project` to teach the flag (e.g. "use
  `--skip-member <name>` for a contractually-pinned member").
- Everything else is unchanged: `--no-tag`, `--tag-only`, gate detection, the tag-safety
  behavior. Only `validate_project` and its call site (`src/main.rs:108`/`:581`) change;
  `--tag-only` already returns before the guard and needs no flag.

## Acceptance criteria

- No flag: behavior is unchanged (independent member still aborts).
- `--skip-member claude-pricing` lets clyde's normal and `--no-tag` flows past validation.
- Only `[workspace.package].version` is bumped; `pricing/Cargo.toml` is byte-for-byte
  untouched.
- An independent member not named by `--skip-member` still aborts **before** any mutation.
- A `--skip-member` naming a nonexistent or non-independent member aborts before any mutation.
- `--tag-only` is unchanged and needs no skip flag.
- The skip is printed to the terminal (verify under the normal and `--no-tag` flows).
- Tests cover: CLI parse (space-separated, repeated), the validation filter, terminal
  visibility, and the normal / `--no-tag` / unchanged `--tag-only` paths.

## Out of scope

- **Independently bumping the pinned member.** `bump` owns the single workspace version line
  only; a member that pins its own version is managed by hand, by design.
- **Auditing dependent version requirements.** `bump` does not maintain `version = "..."`
  constraints that other members place on the pinned crate. Leaving the pin stale is safe for
  clyde specifically because its `claude-pricing` consumers use path-only deps
  (`report/Cargo.toml`, `cost/Cargo.toml`) with no version constraint; a workspace that pins a
  member *and* version-constrains it elsewhere is the maintainer's responsibility, not
  `bump`'s.

## Resolved constraint: never tag the pinned member with a `v`-prefixed name

`bump` discovers the base version to bump from with `git tag -l "v*" --sort=-v:refname`
(`src/git.rs:15`) -- it does **not** derive a version from `git describe` (that appears only
in `head_has_tag`, `src/git.rs:110`, as a boolean "is HEAD tagged" check that parses nothing).

Consequence for the hand-managed pinned member: if `claude-pricing 2.1.0` is ever tagged in
this repo as `v2.1.0`, that tag matches the `v*` glob and sorts **above** the workspace's
`v0.5.x` line, so `get_latest_tag` returns `v2.1.0` and `bump` bumps the workspace from the
wrong base (next bump jumps to v2.1.1 / v3.0.0). This is also exactly the per-crate /
multi-scheme tag strategy the repo's git rule forbids ("Always use a single flat `v*` tag for
the whole repo/workspace").

So the pinned member must **not** be tagged with a `v`-prefixed name here. Any non-`v*` name
(`claude-pricing-2.1.0`, bare `2.1.0`) is invisible to `bump`'s discovery and safe; better
still, a crate consumed by path / crates.io needs no repo tag at all. No code change is
required for this -- it is a naming constraint on the hand-managed release, recorded so the
collision is not rediscovered later.
