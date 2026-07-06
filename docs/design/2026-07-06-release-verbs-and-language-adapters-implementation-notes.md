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

## Phase 1: Language-adapter seam (zero behavior change)

### Design decisions
- New module entry point `src/lang.rs` + submodules `src/lang/cargo.rs` (moved from
  `src/cargo.rs` via `git mv`) and `src/lang/python.rs` (moved from `src/python.rs`
  via `git mv`) -- Rust 2018+ style per `rules/rust.md` ("the module entry point is
  `foo.rs`, submodules live in `foo/` alongside it"), matching the repo's existing
  flat top-level `mod x;` convention at the outer layer while giving the two adapters
  a real submodule home.
- All 6 match sites (`read_file_version`, `write_file_version`, `sync_lockfile`,
  `version_file_name`, `is_version_files_only`, `validate_project` -- `main.rs:51,66,
  81,90,99,127` pre-move) moved verbatim into `src/lang.rs`, plus `detect_project_type`
  (main.rs:40, the detection match itself) and the `ProjectType` enum + its `Display`
  impl and the `skip_message` helper `validate_project` depends on. `main.rs` now only
  calls plain functions from `lang::`; it contains zero remaining matches on
  `ProjectType` for language-specific behavior — `determine_version_action` and
  `determine_commit_message` still branch on `ProjectType` for POLICY (the
  untouched-default-0.1.0 rule, generic-vs-managed needs_file_update), which the design
  doc's Data Model explicitly keeps out of the adapter/trait layer ("Language-specific
  version POLICY ... stays in the adapter/policy layer, not the trait" -- doc line 138).
- `src/lang.rs:skip_message` is `pub(crate)` (not private) because
  `main.rs`'s existing inline test `skip_message_names_member_and_version` calls it
  directly; `main.rs` re-imports it under `#[cfg(test)] use lang::skip_message;` so the
  binding is visible to the `tests` submodule's `use super::*;` without adding an
  always-on unused-import warning to the release build.
- New test `lang::cargo::tests::test_check_independent_versions_glob_member_silently_
  skipped` (`src/lang/cargo.rs`) documents the design doc's Risks-table entry
  ("Workspace glob members (`crates/*`) silently skipped by independent-version scan,
  cargo.rs:234-236"): a `members = ["crates/*"]` workspace with a real independently-
  versioned member crate behind the glob still returns an empty
  `Vec<IndependentVersionMember>`, because `check_workspace_independent_versions`
  treats every `members` entry as a literal path and `continue`s when
  `dir.join(member_path).join("Cargo.toml")` doesn't exist (it never expands the
  glob). The test asserts today's (buggy) behavior, not a fix — no fix lands in this
  phase, per the risk table's own mitigation ("fix folded into Phase 4 if any target
  repo uses globs").

### Deviations
- Did NOT introduce the `ManifestVersion` enum / `Manifest` trait / `Vec<Box<dyn
  Manifest>>`-returning `detect()` from the doc's Data Model section. The doc's own
  Phase 1 plan bullet only asks to "Extract the 6 match sites ... Detection still
  Rust-wins internally -- behavior identical, seam in place," and explicitly defers
  the Vec-returning multi-manifest `detect()` to Phase 3 ("Multi-manifest detection:
  `detect` returns all ROOT-LEVEL version-bearing manifests"). Building the trait/dyn
  object machinery now, with only Rust and Python (both already dispatched
  identically via `ProjectType`) exercising it, would be premature abstraction with no
  second real caller yet — Node is the second adapter that will actually need the Vec
  shape. Same effect (all language dispatch lives behind one module boundary,
  `main.rs` has zero match sites on it), correct seam for THIS phase's stated scope;
  the richer trait lands in Phase 3 when it has two adapters to justify its shape.
- `detect_project_type` (main.rs:40) was folded into the seam even though the phase's
  bullet list names only the 6 downstream match sites — it is the detection match
  itself (`cargo::cargo_toml_exists` / `python::pyproject_toml_exists`), and leaving it
  in `main.rs` while its two callees moved to `lang::` would have left one match site
  outside the boundary the phase exists to close. Same effect as the doc's intent,
  correct seam.

### Tradeoffs
- Plain functions (`read_file_version`, `write_file_version`, etc.) over trait
  objects: preserves byte-identical behavior with a minimal diff (188 lines removed
  from `main.rs`, no new abstraction to prove correct) and satisfies the phase's actual
  success criterion ("adding a new language requires zero new match sites outside
  `src/lang/`") — a Node adapter added under this shape needs one new arm per function
  in `lang.rs`, all inside the module boundary, none in `main.rs`. Chose this over
  introducing `dyn Manifest` now, which would add indirection with no behavioral or
  test benefit until Phase 3's Vec-detection actually consumes it.
- Left `src/lang/cargo.rs` and `src/lang/python.rs` test bodies exactly as they were
  (inline `#[cfg(test)] mod tests { ... }`, not extracted to `src/lang/cargo/tests.rs`
  per `rules/rust.md`'s test-file-placement convention) — the phase's hard constraint
  is "zero edits to EXISTING tests," and a `git mv` that also reorganizes test file
  layout is churn this phase is explicitly told to avoid ("do not reorganize them into
  separate files this phase"). The new glob-skip test was appended to the same inline
  module for the same reason: matching the file's current convention, not introducing
  a second one mid-move.

### Open questions
- None.

## Phase 2: Python fixes

### Design decisions
- uv.lock sync lives in `sync_lockfile` -> `sync_lockfile_with` (src/lang/python.rs):
  when `uv.lock` exists, run `uv lock`; success = trued-up lock, non-zero exit = loud
  error, `uv` binary NotFound while `uv.lock` present = loud error (never a silently
  stale lock). No `uv.lock` = no-op. `poetry.lock` stays untouched (does not record
  the root package version -- verified Phase 0).
- Lockfile guard mechanism (src/lang.rs:`is_version_files_only`, src/git.rs:`dirty_files`,
  src/main.rs `run`): the pre-bump working-tree dirty set is captured with
  `git::dirty_files` BEFORE any mutation and threaded through
  `determine_commit_message` into `is_version_files_only(staged, type, predirty)`. A
  synced lockfile (`Cargo.lock`/`uv.lock`, from `synced_lockfiles`) counts toward
  version-only ONLY when it is NOT in the pre-bump dirty set -- i.e. bump's own sync
  produced it on a clean tree. A pre-dirtied lockfile (user dep changes) fails the
  check and drops through to the editor prompt, so it can never be silently folded
  into an auto "Bump version to X" commit. Manifest files (`Cargo.toml`/`pyproject.toml`,
  from `manifest_files`) are always version-files; the guard is lockfile-scoped, matching
  the phase bullet.
- Dynamic-version refusal at the WRITE path (src/lang/python.rs:`write_version` +
  `has_dynamic_version`): `read_version` maps both genuinely-missing and dynamic to
  `None` (unchanged); they diverge at write. `write_version` bails when
  `dynamic = ["version"]` is present, with a message that names `dynamic = ["version"]`,
  and never touches the file. Plain-absent (no `dynamic`) still writes a new
  `[project].version` (today's behavior, preserved).
- `has_dynamic_version` extracted as one helper, used by both `read_version` (replacing
  the old inline loop) and `write_version` -- single source of truth for "is this
  dynamic," so the read/refuse decision cannot drift.

### Deviations
- Phase 1 deferred the `ManifestVersion` enum / `Manifest` trait from the Data Model,
  so Phase 2 is implemented against the current plain-function shape (`read_version`
  returns `Result<Option<String>>`, write dispatch is `lang::write_file_version`). Same
  effect at the correct seam: the "dynamic REFUSES vs missing WRITES" divergence the
  enum was meant to encode is realized at the write path instead of in the type. When
  Phase 3 introduces the enum, this refusal collapses into the `Dynamic` variant.
- `is_version_files_only` signature grew a third parameter (`predirty_files`). No
  existing test called it directly (only `determine_commit_message` does), so no
  existing test needed editing.

### Tradeoffs
- Testable-seam `sync_lockfile_with(dir, uv_bin)` vs PATH mutation in tests: added the
  seam so the "uv missing + uv.lock present" loud-error path is exercised
  deterministically by pointing at a nonexistent binary name, avoiding
  `set_var("PATH", ...)` (unsafe in edition 2024, requires an env-lock, races parallel
  tests). Production `sync_lockfile` calls it with `"uv"`.
- uv-lock-check test is gated (skips, does not fail) when `uv` is absent or the initial
  `uv lock` fails (no interpreter / offline). The fixture is dep-free so `uv lock`
  needs no network to resolve; verified locally with uv 0.7.21 (test ran and passed,
  not skipped). This keeps the default suite green on machines without uv while
  exercising the real round-trip where uv exists.
- Lockfile guard scoped to lockfiles only (not the manifest): the phase bullet and
  panel finding are specifically about a pre-dirtied *lockfile*. A pre-dirtied manifest
  is a separate concern not in this phase's scope.
- New tests kept inline (`#[cfg(test)] mod tests`) in python.rs and lang.rs to match
  the codebase's existing convention rather than introducing `python/tests.rs` /
  `lang/tests.rs` mid-phase (per the phase instruction to match the file's existing
  style). rules/rust.md's separate-file convention is a tree-wide mechanical pass, not
  this phase's job.

### Open questions
- None.
