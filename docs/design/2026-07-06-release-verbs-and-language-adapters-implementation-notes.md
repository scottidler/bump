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

## Phase 3: Node adapter

### Design decisions
- Introduced the deferred abstraction in `src/lang.rs`: the `ManifestVersion` enum
  (`Static(Version)` | `Missing` | `Dynamic(String)`), the `Manifest` trait
  (`path` + `read_version`/`write_version`/`sync_lockfiles`/`version_files`/`validate`),
  `detect() -> Result<Vec<Box<dyn Manifest>>>` (ROOT-LEVEL only, never recursive),
  `agreed_version()` (one repo = one version = one tag; loud refusal on disagreement),
  and `write_all()` (lockstep write + lock sync across every detected manifest).
- Node adapter `src/lang/node.rs`: READ the authoritative top-level version with
  `serde_json::Value` + `value.get("version")` (`node::read_version`); WRITE via a
  targeted string edit of the SHALLOWEST-indent `"version"` line
  (`node::write_version` -> `locate_shallowest_version` / `parse_version_line`),
  cross-checked against the parse and bailing loud on disagreement -- byte-exact, only
  the one line changes. Lock sync via `npm install --package-lock-only`
  (`node::sync_lockfile` -> `sync_lockfile_with(dir, npm_bin)` seam); npm absent while
  package-lock.json present is a loud error; pnpm-lock.yaml/yarn.lock get no sync.
- cargo/python implement `Manifest` (`cargo::CargoManifest`, `python::PythonManifest`),
  each reusing the existing free functions. `python::PythonManifest::read_version`
  re-parses to distinguish `Dynamic` (dynamic = ["version"]) from `Missing`, since the
  free `python::read_version` still maps both to `None`.
- `python::is_version_bearing` (src/lang/python.rs) gates Python detection on a
  `[project]`/`[tool.poetry]` section, so a ruff-config-only pyproject next to
  Cargo.toml does not trigger Python. `detect_project_type` now uses the SAME predicates
  as `detect()` (added `ProjectType::Node`, switched Python to `is_version_bearing`) so
  the policy type and the manifest set never diverge.
- `process_directory` (src/main.rs) now wires the trait end-to-end in production:
  `lang::detect` -> per-manifest `validate` -> `lang::agreed_version` (feeds
  `file_version`, refuses on disagreement/dynamic) -> `lang::write_all` (replaces the
  two `write_file_version` + `sync_lockfile` pairs) writing every manifest in lockstep.
  `ProjectType` stays the POLICY marker for `determine_version_action` (untouched-0.1.0
  default, generic-vs-managed `needs_file_update`) per the doc's Data Model.

### Deviations
- `Box<dyn Manifest>` is used deliberately, an EXPLICIT exception to rules/rust.md's
  generics-over-`dyn` preference -- the design doc's Data Model sanctions it here for
  the heterogeneous cargo/python/node collection `detect()` returns. Noted in the trait
  doc-comment (src/lang.rs).
- The package.json read carve-out: `node::read_version` uses `serde_json::Value` +
  `value.get("version")` rather than a modeled struct, so unmodeled package.json fields
  and nested `"version"` keys are tolerated (NO `deny_unknown_fields`, NO struct). Same
  effect as the doc's "read struct without deny_unknown_fields" with one fewer dep
  (serde derive is not pulled a phase early; serde arrives with config in Phase 4).
- Added `path()` to the trait (not in the doc's trait sketch) so `agreed_version` can
  name each offending file in the disagreement error -- same intent, needed seam.
- Removed the now-dead `lang::write_file_version` and `lang::sync_lockfile` dispatch
  wrappers (production now writes via `write_all`); no test called them directly, so no
  test churn. `read_file_version` / `version_file_name` are kept (still used by
  `tag_only` and `determine_version_action`).
- Node bumping via bare `bump` works through the Generic-adjacent path: a node-only repo
  reads its version via `agreed_version` and `determine_version_action` runs under
  `ProjectType::Node` (needs_file_update true), so `write_all` bumps package.json.
  Full node-flow polish is exercised by the release verbs (Phases 5-7) that consume
  `detect()`; this phase delivers + tests the adapter and multi-manifest machinery.
- `is_version_files_only` / `agreed_version` / `write_all` are wired into production
  precisely because a bin crate flags test-only `pub`/trait methods as dead_code under
  `-D warnings`; every trait method is reached via a `dyn` call in `process_directory`.

### Tradeoffs
- Trait `validate` delegates to the existing `validate_project` (CargoManifest -> Rust,
  PythonManifest -> Python, NodeManifest -> Node), preserving the tested workspace
  independent-version + stale-skip-member behavior exactly for single-manifest repos.
  A hypothetical dual cargo+python repo passing a cargo `--skip-member` would have the
  python manifest's validate refuse it (fail-closed, not corruption); no such repo
  exists in the fleet, and refusing is the safer direction. Single-manifest behavior
  (the only real/tested case) is byte-identical.
- The real npm round-trip test (`npm_round_trip_updates_both_lock_sites`,
  src/lang/node/tests.rs) GATES: it returns early (skips, never fails) when `npm` is
  absent, when the initial `npm install --package-lock-only` errors (sandbox EROFS /
  offline), or when no package-lock.json is produced -- so the default sandboxed
  `otto ci` stays green. In THIS environment npm was available and could write the
  lock, so the test RAN and PASSED (verified both root sites: top-level `version` and
  `packages[""].version` update to 0.1.1). The deterministic coverage that ALWAYS runs
  carries the mechanic: byte-exact 1-line targeted write incl. a nested `"version"`
  key, shallowest-not-first anchoring, and the npm-missing loud error via the
  nonexistent-binary seam.
- New node tests live in `src/lang/node/tests.rs` (rules/rust.md 2018+ placement) since
  node.rs is a new file. New multi-manifest tests were appended INLINE to `src/lang.rs`'s
  existing `#[cfg(test)] mod tests` to match that file's current convention (Phase 1/2
  kept the inline style there; a tree-wide extraction is a separate mechanical pass).

### Open questions
- None. (Cross-repo/system-mutating steps like retiring the bash driver are Phase 9,
  not executable here; no such work was needed for Phase 3.)

## Phase 4: Repo-local facts config

### Design decisions
- `src/config.rs`: `Config { skip_members: Vec<String>, install: Option<String> }`,
  `#[serde(rename_all = "kebab-case", deny_unknown_fields)]` (`skip-members`,
  `install` in YAML). `load(dir: &Path) -> Result<Config>` reads `<dir>/bump.yml`;
  missing file returns `Config::default()` (not an error) and logs at `info!` that
  none was found; a present file logs at `info!` which path loaded and at `debug!`
  the parsed `skip_members`/`install`.
- Deliberate exception to `rules/rust.md`'s "never load config from CWD as a silent
  fallback": `load` is documented in the module doc-comment as the doc-sanctioned
  repo-COMMITTED facts file (same trust model as `.otto.yml`), loaded from the root of
  the directory bump is PROCESSING (`process_directory`'s `dir` argument), never XDG,
  never a re-rooted CWD search. This matches the design doc's Resolved Decisions
  ("install is a repo-committed config fact").
- Precedence wired in `config::effective_skip_members(cli_skip_member, &config)`
  (`src/config.rs`): CLI flag non-empty -> use it wholesale (never merged with
  config); flag empty -> config's `skip_members`; both empty -> `vec![]`. Called from
  `process_directory` (`src/main.rs`) right after `lang::detect`, feeding both the
  `manifests.is_empty()` (`validate_project`) and non-empty (`m.validate`) branches --
  the single seam both paths already shared for `cli.skip_member`.
- `install` is loaded and exposed (`repo_config.install`) but not otherwise consumed
  this phase, per the bullet ("this phase just defines/loads/exposes it"); it is
  `debug!`-logged when present in `process_directory` so the field is genuinely read
  (a bin crate's `-D warnings` flags an unread pub struct field as dead_code) without
  inventing release-verb behavior that belongs to Phase 5-7.
- The unknown-key error is surfaced from `serde_yaml`'s own `Display` embedded
  directly into the top-level `eyre::eyre!` message (`config::load`), not left in the
  eyre context chain -- `eyre::Report::to_string()` (and `process_directory`'s error
  path, which callers read via `.to_string()`/`{:#}`) shows only the top message by
  default, so the offending key name (`deny_unknown_fields`'s payload) must live
  there to be assertable without unwrapping a Debug chain.

### Deviations
- None against this phase's bullet. `--install`/`--no-install` CLI flags are
  explicitly NOT added here (the bullet reserves them for Phase 5); only the config
  key is loaded and made available.

### Tradeoffs
- Config tests live in `src/config/tests.rs` (rules/rust.md 2018+ test-file
  placement, since `config.rs` is a new module) -- unlike Phase 1-3's carve-out for
  pre-existing inline `mod tests` blocks, there is no existing inline convention to
  match here, so the tree-wide-standard placement applies directly.
- Kept the integration-level `bump.yml` tests (`config_skip_members_allows_pass_
  without_flag`, `config_skip_members_overridden_by_cli_flag`,
  `config_unknown_key_aborts_before_mutation`) inline in `src/main.rs`'s existing
  `#[cfg(test)] mod tests`, matching that file's established convention (same
  reasoning as Phase 1-3's `main.rs`/`lang.rs` test placement) -- only the genuinely
  NEW module (`config.rs`) gets the 2018+ file split.
- `create_independent_workspace` (`src/main.rs`, pre-existing from the
  `--skip-member` phase) doubled as the "clyde-style workspace fixture" the success
  criteria call for, rather than writing a second near-duplicate fixture builder.

### Open questions
- None. (Deleting the stale `bump.yml` sample and replacing it with
  `bump.yml.example` was executed directly -- it is a repo-local file rename/rewrite,
  not a cross-repo/system-mutating step.)

## Phase 5: `bump release` -- ungated flow

### Design decisions
- `src/release.rs` is a callable module with `release(dir, opts, pusher, installer) ->
  Result<ReleaseReport>` (`release::release`), split into `classify()` (typed state
  detection, read-only except the preconditional `git fetch origin <default>`) and
  `execute()` (turns each typed `ReleaseState` into the correct mutation sequence or a
  refusal). Every decision is on typed internals -- `github::detect`/`Gate`,
  `git::compare_head_to_remote`/`HeadRemote`, `lang::detect`/`agreed_version`/
  `ManifestVersion`, `determine_version_action`/`VersionAction` -- ZERO stdout parsing.
- The internal version commit reuses the existing `--no-tag` code path verbatim:
  `version_commit()` (`src/release.rs`) constructs a `Cli { no_tag: true, .. }` and calls
  `crate::process_directory`. No commit/version/tag logic is reimplemented.
- Strengthened ordering enforced in code (`execute_release`, `src/release.rs`): version
  commit -> `pusher.push_branch` -> `confirm_on_origin` (re-fetch + require `HEAD ==
  origin/<default>`) -> `git::create_tag` -> `pusher.push_tag`. The tag is created ONLY
  after the branch is confirmed on origin, so a rejected branch push can never strand a
  local tag on an unpushed commit -- a deliberate strengthening over plain-bump's
  tag-local-HEAD-then-push ordering.
- RESUME (`classify_equal`, `src/release.rs`): when `HEAD == origin/<default>` and the
  manifest version's remote tag is ABSENT (`git::remote_tag_sha(..).is_none()`), it is a
  partial release. `local_tag_present` (`git::tag_exists` + `git::tag_sha == HEAD`) splits
  the two sub-states: absent -> create the annotated tag then push; present -> push only.
  Never re-bumps (never calls `version_commit`), never prints "already released".
- Refusals are typed `ReleaseState` variants; `execute()` `bail!`s each with the EXACT
  next command: NotOnDefault -> "git checkout <default>", Behind|Diverged -> "git pull
  --ff-only origin <default>", Nothing -> "nothing ahead ... already tagged", DirtyTree,
  DetachedHead, Unknown (carries the probe reason), Gated (deferred to Phase 6).
- Gate::Unknown FAILS CLOSED (`classify` -> `ReleaseState::Unknown` -> refuse) because
  `release` pushes -- the opposite of plain `bump`'s warn-and-proceed (which never pushes).
- New `git.rs` helpers `push_branch` / `push_tag` push a single ref to origin BY EXPLICIT
  NAME (never `--tags`/`--follow-tags`/`--force`). Production `GitPusher` delegates to them.
- Install step (`resolve_install`, `src/release.rs`): precedence override
  (`InstallChoice::Command`) > config `install` > default `cargo install --path .` iff a
  `Cargo.toml` is present > skip (`None`). Resolution is pure and separate from execution
  (`run_install` runs it via the injected `Installer`); the resolved command is returned in
  `ReleaseReport.install_command`. Test mechanism: a `RecordingInstaller` double captures
  the resolved command WITHOUT executing it (no real `cargo install` in any test); one
  test exercises the production `ShellInstaller` with `InstallChoice::Command("touch
  install-marker")` and asserts the marker file appears (proves real execution) -- a
  harmless local command, never an outward/slow install.
- `-n` dry-run (`execute_release`/`execute_resume`): echoes every command it would run
  (`[dry-run] ...`) and executes NOTHING -- no `version_commit`, no push (ports untouched),
  no tag, no install. Returns a `ReleaseReport { dry_run: true }` carrying the would-be tag
  and resolved install command.
- DI via a small `Ports<P: Pusher, I: Installer>` bundle (generics, not `dyn` --
  rules/rust.md), keeping the execution functions under the clippy argument-count limit.

### Deviations
- The `release` module is gated `#[cfg(test)]` this phase (same for the two new
  `git::push_branch`/`push_tag` helpers). bump is a bin crate and CI runs `cargo clippy
  --all-targets -- -D warnings`, under which any item unreachable from `main` is
  `dead_code` (Phase 3's notes document this exact constraint). With CLI wiring explicitly
  deferred to Phase 8, the only reachable callers this phase are the tests. Gating keeps
  the module fully compiled AND fully tested (clippy `--all-targets` builds the test
  target) without shipping unreachable code. Same effect (a built, tested, callable state
  machine), correct seam for a not-yet-wired scaffold; Phase 8 removes the gate when it
  adds the subcommand and calls `release()` from production. Flagged as an open question
  for the parent.
- API Design table says the exact-next-command messages; implemented as typed
  `ReleaseState` refusal variants each `bail!`ing its exact command (same effect, typed
  seam) rather than free-form strings, so each refusal is both individually classifiable
  and asserted in a dedicated test.
- Test assertions use `git::remote_tag_sha(..).is_some()` (tag present on origin), NOT
  `== Some(head_sha)`: `git ls-remote origin <exact-refspec>` does not emit the peeled
  `^{}` line, so `remote_tag_sha` returns the annotated tag-OBJECT SHA for an exact
  refspec. Production `classify` only ever uses `.is_some()` for the released/not-released
  decision, so this is correct there; only the positive test assertions were adjusted.
- `main.rs`: `determine_version_action`, `process_directory`, and `VersionAction` (+
  fields) widened from private to `pub(crate)` so the sibling `release` module can reuse
  them. No behavior change; both are still reached from production `main`, so no new
  dead_code. The `#[cfg(test)] static ENV_LOCK` was hoisted from inside `mod tests` to the
  crate root (`pub(crate)`) so `main.rs` gate tests and `release` tests share ONE lock for
  the process-global `BUMP_GATES_PROBE` env var (a different lock per module would race).
  Existing test bodies are unchanged (they reach it via `use super::*`).

### Tradeoffs
- Reused `process_directory` in `--no-tag` mode for the version commit (vs. inlining a
  focused write+stage+commit) -- faithful to the doc's "the internal version commit is
  the existing `--no-tag` code path" and avoids duplicating the amend-vs-new-commit logic.
  Cost: it prints its own "Run: git push ..." hint (worded for the primitive flow); cosmetic
  and invisible until Phase 8 wires the verb, noted for cleanup then.
- Order assertion via a `RecordingPusher` that records call order AND performs the REAL
  push (so `confirm_on_origin` genuinely passes), plus a `fail_branch` variant that records
  then errors WITHOUT pushing for the rejected-push test. This gives a direct `["branch:main",
  "tag:v0.1.6"]` order assertion and a rejected-push assertion (zero tags, tag push never
  attempted) without a fake git layer.
- Generic (no-manifest) ungated repos: fresh release works (tag-only semantics via
  `determine_version_action`); a generic repo with `HEAD == origin` classifies as
  `Nothing` (no manifest version to resume-tag). A `Missing`/`Dynamic` agreed version at
  `HEAD == origin` maps to `Nothing`/refuse respectively -- edge handling noted, not a
  documented row.

### Open questions
- Confirm Phase 8 removes the `#[cfg(test)]` gate on `mod release` and on
  `git::push_branch`/`push_tag`, wires the `bump release` subcommand + `--install`/
  `--no-install` flags (constructing `ReleaseOpts`), and calls `release(dir, &opts,
  &GitPusher, &ShellInstaller)` -- these are the only steps that turn this phase's
  test-built scaffold into shipped, main-reachable code.

## Phase 6: `bump release` -- gated flow

### Design decisions
- Extended the Phase 5 state machine with the GATED rows on typed state -- `classify`
  now routes a `Gate::Gated(_)` verdict into `classify_gated` (`src/release.rs`) instead
  of the Phase-5 "not this phase" placeholder. `classify_gated` resolves+fetches the
  default (like the ungated path) then splits: on the default branch it is a stranded
  refusal (`HeadRemote::Ahead`/`Diverged` -> `GatedStranded`), a behind refusal (reuses
  the ungated `Behind` row), or the "bump rides a PR" refusal (`Equal` ->
  `GatedDefaultClean`); on a feature branch `classify_gated_feature` decides fresh vs
  already-bumped vs level-mismatch vs generic-unsupported.
- Fresh-vs-already-bumped discriminator (`classify_gated_feature`, `src/release.rs`):
  compares the agreed manifest version `V` to the last released tag `T`. `V == T` (or a
  Rust untouched-default 0.1.0, which defers to the tag) -> `GatedFresh`, target computed
  by bump's own `compute_target_tag`/`determine_version_action` (no re-derivation). `V !=
  T` and not untouched-default -> a prior gated `--no-tag` bump already rode the branch;
  if the requested level's implied version (`bump_version(T, level)`) equals `V` it is an
  idempotent `GatedAlreadyBumped`, otherwise `GatedLevelMismatch { riding, implied }`.
- `execute_gated` (`src/release.rs`) is the shared fresh/already-bumped executor: for
  fresh it runs the internal `--no-tag` `version_commit` (reused verbatim from Phase 5 --
  the version bump rides the branch, NO tag); for a re-run it skips the re-bump; then in
  BOTH cases it pushes the feature branch (`--no-follow-tags -u`) and ensures an OPEN PR
  (list-probe -> create-only-if-none), then prints the pause message and returns a
  `ReleaseReport { paused: true, install_command: None }` (exit 0). No tag, no install
  in any gated path -- those are `bump finish`'s (Phase 7).
- The open-PR probe uses `gh pr list --head <branch> --state open --json number`
  (`github::pr_list_args` + `open_pr_exists`, `src/github.rs`), NOT `gh pr view` -- see
  Deviations. `create_pr` is `gh pr create --fill`, only reached when the probe returns
  empty. The JSON verdict is a pure helper (`open_pr_exists_from_json`): non-empty array
  = open PR (skip create), empty array/empty stdout = none (create), any non-array/non-
  JSON = a loud error (never a silent false).
- New `Pr` port (trait) with production `GhPr` and a test `RecordingPr`
  (`src/release.rs`, `src/release/tests.rs`), mirroring Phase 5's `Pusher`/`Installer`:
  `open_pr_exists(dir, branch) -> Result<bool>` and `create_pr(dir, branch) ->
  Result<()>`. Threaded through `release(dir, &opts, &pusher, &installer, &pr)` via the
  Phase-5 `Ports` bundle, now `Ports<'a, P: Pusher, I: Installer, R: Pr>` (rules/rust.md
  generics-over-`dyn`; the `Deps`-style bundle keeps the execution fns under the arg-count
  limit).
- New git helper `git::push_feature_branch` (`git push --no-follow-tags -u origin
  <branch>`) is SEPARATE from Phase 5's `push_branch` so the gated `--no-follow-tags`
  invariant cannot leak into the ungated default-branch push, and vice versa. Gated
  `#[cfg(test)]` like `push_branch`/`push_tag`.
- Stranded rescue prints the LITERAL runnable commands (`git branch stranded-<sha8>`,
  `git reset --hard origin/<default>`, `git checkout stranded-<sha8>`, `bump release`) --
  never a prose description; the suggested branch name is derived deterministically from
  the stranded HEAD's short SHA (`suggest_rescue_branch`) and the verb NEVER creates a
  branch or resets history itself (panel consensus, replacing the bash driver's
  auto-rescue).
- `ReleaseReport` gained a `paused: bool` field (true only on a gated pause); the four
  Phase-5 construction sites set `paused: false`. Phase 5 tests read fields individually,
  so the added field did not touch their assertions.
- `DEFAULT_UNTOUCHED_VERSION` (main.rs) widened private -> `pub(crate)` so the gated
  feature-branch classifier reuses the one untouched-default constant instead of inlining
  a magic `0.1.0` (still used by production `determine_version_action`, so no new dead
  code).

### Deviations
- PROBE SUPERSEDES THE API DESIGN TABLE: the table's gated rows say "`gh pr view`
  existence check FIRST"; the Phase 0 addendum ("gh open-PR probe") supersedes that with
  `gh pr list --head <branch> --state open --json number` -- `gh pr view` returns exit 0
  for a MERGED PR and cannot distinguish open from merged, so a reused branch name would
  falsely read as "PR exists". Implemented the list-probe (same effect, correct/observed
  seam). `gh pr create --fill` is the race backstop behind the probe (known to error on
  an existing open PR; not live-observed, per the addendum).
- PORT over env seam: the doc offers a `BUMP_PR_PROBE` env seam "matching the gates
  precedent"; chose the DI `Pr` port for consistency with Phase 5's `Pusher`/`Installer`
  (the phase instruction's stated preference), no concrete blocker hit.
- Refusal MESSAGES implemented as typed `ReleaseState` variants each `bail!`ing its exact
  command (same seam as Phase 5's ungated refusals), rather than free-form strings, so
  each row is individually classifiable and asserted in a dedicated test.
- Added a `GatedGeneric` refusal row NOT in the phase's 4-row list: a gated repo with no
  manifest is unsupported per Resolved Decisions ("both verbs refuse on gated+generic").
  This is the fail-closed behavior (bump finish cannot derive a version); without it a
  gated generic repo would fall into the fresh path and bump nothing. Same intent as the
  doc's resolved decision, surfaced as an explicit typed refusal + test.
- Repurposed Phase 5's now-stale `refuses_when_gated_this_phase` test (which asserted the
  "gated flow is a later phase" refusal) into `gated_on_default_clean_refuses_bump_rides_
  a_pr`, asserting the new on-default-clean row -- the behavior it pinned by name is gone,
  so the test was inverted to the new behavior rather than left green by accident.

### Tradeoffs
- Reused `version_commit` (`--no-tag` `process_directory`) for the fresh gated bump: on a
  clean tree with an unpushed feature commit it AMENDS the caller's code commit with the
  version bump (Phase 5's mechanic). The version bump still rides the branch in one
  commit, which satisfies "version commit joins the branch"; chose this over a separate
  bump commit to avoid duplicating the amend-vs-new-commit logic.
- `RecordingPr` models real `gh` behavior (after `create_pr`, `open_pr_exists` returns
  true) so "create exactly once across two runs" is assertable with ONE instance shared
  across both `release` calls -- first run creates (probe empty), second run sees the open
  PR via the probe and skips. This tests the idempotency without a real `gh` or a fake-gh
  PATH shim.
- `GhPr` production impl is compiled but never invoked by tests (no real `gh`/network):
  it is constructed once (in the ungated real-pusher test, whose path never touches the
  PR seam) to satisfy dead-code, and its command/JSON seams are covered by pure unit tests
  in `src/github.rs` (`pr_list_args` argv == the list-probe; `open_pr_exists_from_json`
  for empty/non-empty/non-array/empty-stdout). This is the "test request-building, not
  just the happy path" seam without an outward gh call.
- New gh helpers (`open_pr_exists`, `create_pr`, `pr_list_args`,
  `open_pr_exists_from_json`) and `git::push_feature_branch` are `#[cfg(test)]`-gated (bin
  crate, `-D warnings` flags anything unreachable from `main`), consistent with how Phase
  5 gated `mod release` and `git::push_branch`/`push_tag`. Phase 8 ungates and wires the
  real `GhPr`.
- New gated tests appended INLINE to `src/release/tests.rs` (the module's existing
  test-file, already the 2018+ submodule split); new gh-seam tests appended to
  `src/github.rs`'s existing inline `#[cfg(test)] mod tests` to match that file's current
  convention (a tree-wide extraction is a separate mechanical pass, not this phase's job).

### Open questions
- Cross-repo/system-mutating steps (retiring the bash driver, re-pointing skills/agent,
  the git-release-guard hook allowing the two verbs) are Phase 9 -- NOT executable here.
  Phase 7 (`bump finish`) owns the tag-on-merge step this flow pauses for; nothing in
  Phase 6 pre-empts it.
