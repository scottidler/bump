# Implementation Notes: release verbs + language adapters

Running record of how the implementation diverges from or interprets the design
doc (`2026-07-06-release-verbs-and-language-adapters.md`). Append-only, one section
per phase.

## Phase 0: Prove the three unverified mechanics

### Design decisions
- Ran inline by the orchestrator (not a phase-implementer) -- Phase 0 is a zero-code
  spike whose deliverable is a design-doc addendum, and its findings brief later
  phase agents. Recorded as a `docs:` commit, no source/tests.

### Deviations
- Two design assumptions were corrected by observation (both folded into the doc
  addendum, both change downstream phases):
  - package.json write mechanic: chosen = targeted string edit (write) + serde_json
    (read/validate), NOT serde_json round-trip. serde_json reformats non-2-space
    files, drops the trailing newline, and un-escapes `\uXXXX`.
  - gh open-PR probe: chosen = `gh pr list --head <branch> --state open`, NOT
    `gh pr view` -- `gh pr view` returns exit 0 for a MERGED PR and cannot tell open
    from merged. The API Design table's "`gh pr view` existence check" is superseded.

### Tradeoffs
- Did NOT live-observe `gh pr create --fill` erroring on an existing open PR: it
  requires creating a real open PR (outward-facing) for well-established behavior that
  sits behind the open-PR probe as a race backstop only. Recorded as known-not-observed.

### Open questions
- None.
