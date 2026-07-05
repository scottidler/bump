# Support workspaces with an independently-versioned member

**Date:** 2026-07-05
**Status:** Proposed

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

Result: `bump` (and `bump --no-tag`, `bump --tag-only`) can never run on clyde. The
release has to be done by hand, which is how a release-branch anti-pattern crept in
(see `~/HALL-OF-SHAME.md`, 2026-07-05).

## Options

1. **Change claude-pricing** so it is not independently versioned. Rejected: the 2.0.0
   pin is a deliberate contract with the pricing feed; unifying it breaks feed
   compatibility.
2. **Teach `bump` to tolerate an independently-versioned member (chosen).** The guard
   becomes opt-out instead of fatal.

## Proposed design

- Add a flag `--allow-independent` (accept members that pin their own version) and/or
  `--skip-member <name>` (repeatable, name specific members to leave untouched).
- With the flag set, the up-front abort becomes: bump `[workspace.package].version`, and
  leave any member with a literal `version =` untouched.
- Emit an INFO line naming each skipped member and its pinned version, so the skip is
  visible in output and never silent, e.g.:

  ```
  skipping claude-pricing (independent version 2.0.0)
  ```

- Everything else is unchanged: `--no-tag`, `--tag-only`, gate detection, the
  tag-safety behavior.

## Out of scope

- Independently bumping the pinned member. `bump` owns the single workspace version line
  only; a member that pins its own version is managed by hand, by design.
