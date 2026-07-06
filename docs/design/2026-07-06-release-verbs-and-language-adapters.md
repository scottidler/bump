# Design Document: release verbs + language adapters

**Author:** Scott Idler
**Date:** 2026-07-06
**Status:** In Review
**Review Passes Completed:** 5/5

## Summary

Fold the external bash release driver (`~/.claude/bin/release`, 212 lines, untested,
drives bump by grepping its stdout) into bump itself as two verbs: `bump release` and
`bump finish`. Put Rust | Python | JS/TS manifest handling behind one language-adapter
seam instead of 6 scattered match sites. Flows stay hard-coded at exactly two
(gated | ungated); a repo-local `bump.yml` carries per-repo FACTS only (skip-members,
install command) -- never flows.

## Problem Statement

### Background

- The release flow is split across three layers; the safety-critical sequencing lives
  in the weakest one:
  - `bump` (Rust, ~150 tests): version edit + commit + tag. Deliberately never pushes
    (2026-06-12 doc, "Pushing... stays in the operator's hands").
  - `~/.claude/bin/release` (bash, zero tests): gate verdict via
    `grep -qiE 'Gates:.*none|ungated'` on `bump --gates` output (release:118-131),
    target version via regex on `bump -n` output (release:143-145). Any bump
    output-wording change silently breaks the driver.
  - Skills + release-driver agent: re-document the two flows in prose, which invites
    agents to improvise raw `bump`/`git` sequences instead of calling the driver.
    2026-07-05 near-miss on clyde: agent merged a PR without the bump riding it, then
    reached for bump-on-main + push-to-gated-main. Every HALL-OF-SHAME entry is an
    agent exercising discretion at a decision point.
- Language support is ad-hoc: `ProjectType {Rust, Python, Generic}` (main.rs:23-27)
  dispatched at 6 separate match sites (main.rs:51,66,81,90,99,127). Adding JS/TS means
  touching all 6.
- Two verified Python bugs found during research:
  - uv.lock records the project's own version (verified in a real uv.lock); bump leaves
    it stale -> `uv lock --check` / `uv sync --locked` CI fails. `python::sync_lockfile`
    is a no-op with a false comment (python.rs:131-133).
  - A `dynamic = ["version"]` project with existing tags hits the `(None, Some(tag))`
    arm (main.rs:399-405) and bump WRITES a static `[project].version` while
    `dynamic = ["version"]` remains -- PEP-621-invalid metadata. Corruption, not refusal.

### Problem

Agents keep botching releases because the flow logic lives in prose (skills) and
untested bash (release script), with bump exposing primitives (`--no-tag`, `--tag-only`)
that hand the decision points back to the agent. And bump can't cover the fleet:
Python support has two real bugs and JS/TS doesn't exist.

### Goals

- One verb per situation, zero decisions for the caller: `bump release` on a branch or
  ungated main; `bump finish` on gated main post-merge. The verb inspects state and
  either executes the one correct sequence or refuses with the exact next command.
- Absorb every step the bash driver performs (pushes, preconditions, PR open,
  stranded-commit rescue, install) using bump's typed internals (`Gate` enum,
  `VersionAction`) -- zero stdout scraping.
- Language-adapter seam: Cargo.toml (workspaces incl. skip-members) | pyproject.toml
  (PEP 621, poetry fallback, uv lock sync) | package.json (npm lock sync). New language
  = one new adapter file, zero new match sites.
- Fix the two Python bugs (uv.lock staleness; dynamic-version corruption -> refusal).
- Repo-local `bump.yml` for per-repo facts: skip-members, install command. Facts only.
- Skills (/bump, /shipit) and the release-driver agent shrink to routing shims; retire
  the bash script.

### Non-Goals

- User-composable or config-defined flows. There are exactly two flows and no third
  (ruling 2026-07-03, reaffirmed 2026-07-06). Config never composes flows. Excluded,
  not parked.
- CI-minted tags / tag-on-merge workflows. Rejected 2026-07-06: bump is the tool,
  agent-executed, no GitHub Actions dependency. Excluded.
- PR merge polling inside bump. `bump release` on a gated repo pauses after opening the
  PR; the release-driver agent owns the wait and runs `bump finish`. bump never polls.
- Publishing (crates.io | PyPI | npm). bump versions and tags; it does not publish.
- Legacy Python version locations (`__init__.py __version__`, `setup.py`). Zero
  references in the fleet's active repos. Parked; revisit only if a real repo needs it.
- npm/pnpm/yarn WORKSPACES and exotic JS lockfiles (bun.lockb, deno). Node adapter v1
  handles a single root package.json + package-lock.json. Parked; revisit when a real
  repo in the fleet needs it (write the seam as if more are coming, implement one).
- Per-crate / multi-scheme tags. Single flat `v*` tag per repo, always (git.md).

## Proposed Solution

### Overview

bump becomes the single home for the whole release mechanic. Two new subcommands wrap
the existing primitives; the primitives (`--no-tag`, `--tag-only`, bare `bump`) remain
for humans but disappear from every skill and agent instruction. Language handling
moves behind a `lang` module boundary with one adapter per ecosystem. A repo-root
`bump.yml` replaces today's stale never-read sample file and carries facts.

### Architecture

- `src/lang/mod.rs` -- adapter boundary. Detection returns ALL version-bearing
  manifests in the repo (Vec, not first-match).
- `src/lang/cargo.rs`, `src/lang/python.rs`, `src/lang/node.rs` -- the three adapters
  (cargo.rs and python.rs move; node.rs is new).
- `src/release.rs` -- the two verbs' state machines. Uses `github::Gate` and
  `VersionAction` directly; executes `git push` / `gh pr create` via the existing
  `run_gh` / command plumbing.
- `src/config.rs` -- repo-root `bump.yml` (serde_yaml, kebab-case keys,
  `deny_unknown_fields`).
- cli.rs gains subcommands; bare `bump` (no subcommand) behaves exactly as today.

### Data Model

Adapter boundary (shape, not signature-level pretend-precision; final form decided in
Phase 1 against the 6 real call sites):

```rust
// src/lang/mod.rs
pub enum ManifestVersion {
    Static(Version),   // normal case: version present, writable
    Missing,           // version-bearing file, no version field yet: writable
    Dynamic(String),   // version owned elsewhere (e.g. dynamic = ["version"]): REFUSE with the reason
}

pub trait Manifest {
    fn read_version(&self) -> Result<ManifestVersion>;
    fn write_version(&self, v: &Version) -> Result<()>;  // errors on Dynamic; never writes corrupt metadata
    fn sync_lockfiles(&self) -> Result<Vec<PathBuf>>;    // files touched, for the commit
    fn version_files(&self) -> Vec<PathBuf>;             // manifest + locks, for is_version_files_only
    fn validate(&self, skip_members: &[String]) -> Result<()>;
}

pub fn detect(root: &Path) -> Result<Vec<Box<dyn Manifest>>>;  // ROOT-LEVEL manifests only
```

- `read_version` returns an enum, not `Option` -- panel finding: `None => refuse` is
  too lossy for the 4 distinct current behaviors (static | missing-but-writable |
  dynamic-refuse | no-manifest-at-all). No-manifest = empty Vec from `detect`.
  `Missing` and no-manifest MUST stay distinct in the policy layer: writable
  missing-version (python.rs:73) and generic-no-manifest (main.rs:400) diverge today,
  and collapsing them resurrects the exact bug the enum prevents (panel note).
  Language-specific version POLICY (e.g. Rust's 0.1.0 untouched-default,
  DEFAULT_UNTOUCHED_VERSION main.rs:317) stays in the adapter/policy layer, not the trait.
- **Detection scope is the repo ROOT only**: `./Cargo.toml`, `./pyproject.toml`,
  `./package.json`. Never recursive -- a nested frontend (`web/package.json`, verified
  to exist in sitr) or a test fixture must not trigger lockstep. Cargo workspace
  members are the cargo adapter's internal concern, not detection's. A repo that needs
  a non-root manifest covered is a future `bump.yml` fact, parked until real.
- pyproject.toml counts as version-bearing only when `[project]` or `[tool.poetry]`
  exists (a ruff-config-only pyproject next to Cargo.toml must not trigger Python).
- Empty Vec = today's `Generic`: no manifest, version lives in tags alone. `bump
  release` on an UNGATED generic repo works (tag-only semantics, next version computed
  from the last tag). Gated generic is UNSUPPORTED: `bump finish` cannot derive a
  target version without a manifest (`--tag-only` already refuses generic repos,
  main.rs:551) -> both verbs refuse on gated+generic with that explanation.
- Dual/tri-manifest repo (all at root): one repo = one version = one tag. All detected
  manifests are written to the same new version in the same commit. Pre-bump version
  disagreement between manifests is a loud error naming both files and values.

Config (repo root `bump.yml`; replaces the stale sample squatting the name):

```yaml
# bump.yml -- per-repo facts for bump. Facts only; flows are hard-coded in bump.
skip-members:            # workspace members with independent (literal) versions
  - claude-pricing
install: cargo install --path .   # run by `bump release`/`finish` after success; omit = no install
```

- Precedence: CLI flags > config file > defaults (general.md). `--skip-member` and
  `--install`/`--no-install` remain as flags and override config.
- Unknown key = loud error (`deny_unknown_fields`).

### API Design

The caller-facing contract. One verb per situation; every refusal prints the exact
next command and exits nonzero.

```
bump release [-m|-M] [-n] [--install "<cmd>"|--no-install]
bump finish  [-n] [--install "<cmd>"|--no-install]
```

Caller contract (unchanged from the bash driver): the CODE CHANGE IS ALREADY
COMMITTED -- the caller owns diff-reading judgment (what to commit, the message) and
branch creation. The verbs own everything mechanical from there. Neither verb ever
authors a code commit or invents a branch name (sole exception: the stranded-commit
rescue, which is recovery of a caller mistake, not a flow).

`bump release` state machine (all preconditions: clean tree, `git fetch origin
<default>` first):

| State detected | Action |
|---|---|
| Ungated, on default, not behind origin, commits to release | version commit (internal `--no-tag`) -> `git push origin <default>` -> confirmed on origin -> annotated tag on HEAD -> `git push origin vX.Y.Z` by name -> install |
| Ungated, not on default | refuse: "checkout <default>, then bump release" |
| Ungated, behind origin | refuse: "git pull --ff-only origin <default>, then bump release" |
| Ungated, nothing to release | refuse: "nothing ahead of origin and version already tagged" |
| Ungated, RESUME: origin/<default> already carries version vX, remote tag vX missing (prior run died between branch push and tag push; local tag may or may not exist) | never re-bump, never claim "already released": create the annotated tag if absent locally, `git push origin vX.Y.Z` by name -> install. Idempotent to completion |
| Gated, on feature branch, version == last tag | `bump --no-tag` internally (version commit joins the branch) -> `git push --no-follow-tags -u origin <branch>` -> PR: `gh pr view` existence check FIRST, `gh pr create --fill` only if absent (`gh pr create` errors on an existing PR -- never assumed clean) -> print "merge the PR, then run: bump finish" -> exit 0, paused |
| Gated, on feature branch, version already bumped (idempotent re-run) | skip re-bump -> ensure branch pushed + PR open (same view-then-create) -> same pause message. If the requested level (`-m`/`-M`) implies a DIFFERENT version than the one riding, refuse naming both -- never silently keep either |
| Gated, on local default with commits not on origin (stranded) | refuse, printing the LITERAL runnable commands -- `git branch <suggested-name>`, `git reset --hard origin/<default>`, then `bump release` on the branch -- never a prose description (printing literal commands is the point of choosing refuse over auto-rescue). The verb never invents a branch or resets history itself -- panel consensus 2026-07-06, superseding the bash driver's auto-rescue |
| Gated, on default, clean, version == last tag | refuse: "bump rides a feature PR; branch, then bump release" |
| Gate Unknown (probe failed, GitHub remote) | refuse with the probe error. `release` pushes; it fails closed. (Plain `bump` keeps warn-and-proceed -- it never pushes.) |
| Dirty tree, detached HEAD, anything unrecognized | refuse with the one exact fix |

`bump finish` (gated repos, after the PR merges):

| State detected | Action |
|---|---|
| Clean tree; origin/<default> carries an untagged version (the merged bump) | checkout <default> -> `git pull --ff-only` -> `bump --tag-only` internally (refuses unless HEAD == origin/<default>) -> annotated tag -> `git push origin vX.Y.Z` by name -> install |
| origin/<default> version == last tag (nothing merged / bump never rode) | refuse: "no untagged version on <default>; bump rides a feature PR -- run bump release on a branch" |
| Tag vX exists on the REMOTE at the merged commit | no-op: "already released vX.Y.Z" -> exit 0 |
| Tag vX exists LOCALLY only (prior run died before/during tag push) | resume: `git push origin vX.Y.Z` by name -> install. A local-only tag is NOT released -- never report it as done (`--tag-only` already distinguishes these, main.rs:565,577; the verb preserves that) |
| Generic repo (no manifest) behind gates | refuse: "gated generic repos are unsupported; finish cannot derive a version without a manifest" |
| Dirty tree | refuse (checkout would clobber or carry strays) with the one exact fix |

finish never asks "did a PR merge" (it cannot know); it asks the observable question:
does origin/<default> hold a version newer than the last tag.

Invariants carried from git.md, enforced in code not prose:

- A tag is created only when the commit it points to is confirmed on origin/<default>.
  This is a deliberate STRENGTHENING over today's plain-bump ungated ordering (which
  tags local HEAD before pushing): `bump release` never holds a local tag on an
  unpushed commit, so a rejected push can no longer strand one -- on any repo kind.
- Tags pushed by explicit name only; never `--tags`, never `--follow-tags`.
- Annotated tags only; only on the default branch; single flat `v*` scheme.
- Never a bump-only release branch. Never force-push. Never tag deletion.

`-n` dry-run echoes every command it would run, executes nothing.

### Implementation Plan

Nine phases in bump + one cross-repo operator phase. Deterministic/cheap first.
(`bump release` was split ungated | gated on panel finding: one commit each.)

#### Phase 0: Prove the three unverified mechanics (zero code)
**Model:** sonnet
- Throwaway uv project: bump `[project].version` by hand, run `uv lock`, confirm the
  lock diff touches only the root package entry.
- Throwaway npm project: confirm `npm install --package-lock-only` updates both
  version sites (root `version` + `packages[""].version`); pick the package.json write
  mechanic (targeted string edit vs serde_json preserve_order + indent detection) by
  testing round-trip fidelity on real-world package.json files.
- Branch with an existing open PR: confirm `gh pr create --fill` behavior (error vs
  no-op) and `gh pr view` as the existence probe, so the view-then-create path is
  built on observed fact.
- **Success criteria:** addendum added to this doc containing, for each of the three:
  the exact commands run, their observed output, and the chosen mechanic -- including
  the package.json write mechanic decision with round-trip diffs on real files. An
  addendum without commands+outputs fails this phase.

#### Phase 1: Language-adapter seam (zero behavior change)
**Model:** sonnet
- Extract the 6 match sites (main.rs:51,66,81,90,99,127) behind `src/lang/mod.rs`;
  move cargo.rs and python.rs to `src/lang/`. Detection still Rust-wins internally --
  behavior identical, seam in place.
- **Success criteria:** full existing suite green with zero edits to EXISTING tests
  (one NEW test documenting the pre-existing workspace-glob skip is added here);
  adding a new language requires zero new match sites outside `src/lang/`.

#### Phase 2: Python fixes
**Model:** opus
- `sync_lockfiles`: run `uv lock` when uv.lock exists; extend `is_version_files_only`
  to accept uv.lock. poetry.lock stays no-op (does not record root package -- verified).
- Lockfile guard (panel finding, applies here and Phase 3): a lockfile counts toward
  version-files-only ONLY when its change came from bump's own sync step on a
  previously-clean tree -- a pre-dirtied lockfile (dep changes) never gets
  misclassified as a version-only change.
- Dynamic-version projects: `read_version` -> None -> the write path REFUSES with a
  message naming `dynamic = ["version"]`, instead of writing corrupt metadata.
- **Success criteria:** regression test for the dynamic+tags corruption case (fails on
  today's code, passes after); uv fixture bump leaves `uv lock --check` green.

#### Phase 3: Node adapter
**Model:** opus
- `src/lang/node.rs`: package.json read/write (mechanic per Phase 0), lock sync via
  `npm install --package-lock-only` when package-lock.json exists; pnpm-lock.yaml /
  yarn.lock need no sync (do not record root version). npm binary absent but
  package-lock.json present = loud error (never a silently stale lock).
- Multi-manifest detection: `detect` returns all ROOT-LEVEL version-bearing
  manifests; disagreement = loud error; all written in lockstep.
- **Success criteria:** bump on an npm fixture updates package.json + both lock sites;
  Cargo+pyproject fixture with mismatched versions refuses with both paths named.

#### Phase 4: Repo-local facts config
**Model:** sonnet
- `src/config.rs`: repo-root `bump.yml`, serde_yaml, kebab-case, deny_unknown_fields;
  keys `skip-members`, `install`. Delete the stale sample. Flags override config.
- **Success criteria:** clyde-style workspace release works with no `--skip-member`
  flag; an unknown config key is a loud error naming the key.

#### Phase 5: `bump release` -- ungated flow
**Model:** opus
- `src/release.rs` scaffold + the UNGATED rows of the state table, on typed internals
  (`Gate`, `VersionAction`) -- zero stdout parsing: clean-tree gate, fetch,
  on-default/behind/ahead checks, Unknown-refusal, version commit -> branch push ->
  tag-after-confirm -> tag push by name, the partial-release RESUME row, install
  step, `-n` echo mode.
- **Success criteria:** ungated e2e against a bare TempDir remote lands branch then
  tag in order; kill-between-pushes fixture asserts BOTH resume sub-states -- local
  tag absent (create+push) AND local tag present (push only) -- since
  `git::tag_exists` (main.rs:685) behaves differently for each; re-run completes
  without re-bumping and without claiming "already released"; each ungated `die`
  condition in the bash script reproduced as a distinct typed error with a test.

#### Phase 6: `bump release` -- gated flow
**Model:** opus
- The GATED rows: internal `--no-tag`, `--no-follow-tags -u` branch push,
  view-then-create PR handling, idempotent re-run + level-mismatch refusal,
  stranded-commits refusal with exact rescue commands, pause message.
- **Success criteria:** gated e2e (BUMP_GATES_PROBE + fake `gh` shim) ends paused with
  branch pushed and PR-create invoked exactly once across two runs; level-mismatch
  and stranded-commits fixtures refuse with the documented messages.

#### Phase 7: `bump finish`
**Model:** opus
- Per the finish table: checkout, `pull --ff-only`, internal `--tag-only` ladder
  intact, tag push by name, install, remote-tag no-op vs local-only-tag resume
  distinction, gated-generic refusal.
- **Success criteria:** e2e resume against a bare remote produces an annotated tag on
  the merged tip; the missed-bump state refuses with the branch instruction; a
  local-only-tag fixture resumes (pushes the tag) instead of no-op'ing; second full
  run is a clean no-op.

#### Phase 8: CLI surface + docs
**Model:** sonnet
- clap subcommand wiring; bare `bump` and all existing flags byte-identical behavior;
  `--help`, README, this doc's Status field.
- **Success criteria:** `bump` bare on the existing test matrix is unchanged; `bump
  release --help` documents the state table.

#### Phase 9: Cross-repo retirement (operator step, in scottidler/claude -- NOT executable by bump phase agents)
**Model:** sonnet
- Retire `~/.claude/bin/release` (rkvr, plus manifest/symlink removal). Shrink
  /bump + /shipit skills to shims ("run `bump release` / `bump finish`; if it refuses,
  do exactly what it prints"). Re-point release-driver agent. Update rules/git.md AND
  the git-release-guard PreToolUse hook (it must allow the two verbs and keep blocking
  raw `git tag` / tag pushes / `--no-tag` on default). Redeploy via manifest so other
  machines pick it up.
- **Success criteria:** grep of skills/agent/rules finds zero references to the bash
  script or to raw `--no-tag`/`--tag-only` instructions; `/bump` skill body <= 15 lines.

## Acceptance Criteria

- [ ] Gated e2e: `bump release` on a feature branch produces exactly one version
  commit on that branch, no tag anywhere, an opened PR, and a pause message naming
  `bump finish`; after merge, `bump finish` creates an annotated `vX.Y.Z` on a commit
  reachable from origin/<default> and pushes it by name.
- [ ] Ungated e2e: `bump release` on default lands branch push then tag push in that
  order against a bare remote; a rejected branch push leaves zero tags (local or remote).
- [ ] Resume e2e: a run killed between branch push and tag push is completed by
  re-running the same verb -- tag pushed, no re-bump, no false "already released";
  same for `bump finish` with a local-only tag.
- [ ] The dynamic-version Python regression test fails on pre-change code and passes
  after (refusal, not corruption); a uv fixture bump leaves `uv lock --check` green.
- [ ] An npm fixture bump updates package.json and both package-lock.json version
  sites in one commit.
- [ ] `~/.claude/bin/release` is retired and /bump + /shipit + release-driver mention
  only `bump release` / `bump finish`; bare `bump` behavior is byte-identical on the
  existing test matrix.

## Resolved Decisions

- 2026-07-06 (Scott, this session): verbs not flags -- `bump release` + `bump finish`
  subcommands; bare `bump` unchanged; primitives remain for humans, vanish from skills.
- 2026-07-03 ruling, reaffirmed 2026-07-06: exactly two flows, hard-coded; missed bump
  rides the NEXT feature PR (no recovery flow, no bump-only branch).
- 2026-07-06 (Scott): YAML-composable flows rejected as "a machine for minting new
  decision points"; config carries facts only.
- 2026-07-06 (Scott): CI-minted tags rejected; bump stays the agent-executed tool.
- 2026-07-06 (research + fail-closed rule): `bump release` refuses on Gate::Unknown
  (it pushes); plain `bump` keeps warn-and-proceed (it doesn't).
- 2026-07-06: one repo = one version = one tag extends to multi-manifest repos --
  lockstep writes, mismatch = loud error. Rust-silently-wins dies.
- 2026-07-06: install command is a repo-committed config fact (same trust model as
  .otto.yml -- you already execute that repo's build); default remains
  `cargo install --path .` iff Cargo.toml, `--no-install` opts out.
- 2026-07-06: merge-wait ownership stays outside bump -- release-driver agent
  babysits the PR and invokes `bump finish`; bump never polls.
- 2026-07-06 (panel consensus, both reviewers): stranded-commits auto-rescue REPLACED
  by refuse-with-exact-commands -- the verb never invents a branch name or resets
  history (deliberate behavior change vs the bash driver's auto-rescue, release:176).
- 2026-07-06 (panel consensus, both reviewers): partial-release resume is a
  first-class state in both verbs -- idempotent to completion, never re-bump, never
  report a local-only tag as released.
- 2026-07-06 (panel finding + verified sitr/web/package.json): manifest detection is
  repo-root only, never recursive; nested manifests are invisible to bump.
- 2026-07-06 (panel finding + verified main.rs:551): gated generic repos are
  unsupported (refuse); ungated generic keeps tag-only semantics.

## Alternatives Considered

### Alternative 1: Keep the bash driver, harden it
- **Description:** add tests + JSON output mode to bump; keep `release` as bash.
- **Pros:** no Rust work; script already handles today's cases.
- **Cons:** still a second home for flow logic; still stdout-coupled; bash is
  untestable at the level this needs; the 2026-06-12 doc already rejected the wrapper
  pattern ("consolidation is the point... bump is the home").
- **Why not chosen:** same argument that killed tagit applies to its successor.

### Alternative 2: CI-minted tags (tag-on-merge workflow / release-plz pattern)
- **Description:** version derived from merge events; CI creates tags; agents never
  version anything.
- **Pros:** zero agent involvement; structurally orphan-proof.
- **Cons:** per-repo GitHub Actions rollout across the whole fleet; PAT plumbing for
  tag-triggered workflows; moves release mechanics off the workstation and out of
  bump.
- **Why not chosen:** Scott rejected it flat (2026-07-06): bump exists, the fleet is
  local-first, and the agent needs one deterministic verb -- not a CI dependency.

### Alternative 3: YAML-composable flow engine
- **Description:** flows defined/composed in XDG config; new edge = new YAML flow.
- **Pros:** flexibility without recompiling.
- **Cons:** mints new decision points; config drift across machines; an agent can
  author a bad flow mid-incident; a DSL for a problem of cardinality two.
- **Why not chosen:** rejected 2026-07-06; new edges get thought about once and
  encoded in Rust with a test (the `--skip-member` precedent).

### Alternative 4: New separate binary for the driver
- **Description:** rewrite `release` as its own Rust CLI calling bump.
- **Pros:** keeps bump single-purpose.
- **Cons:** preserves the two-binary coupling that caused the stdout-scraping; two
  release surfaces to keep in sync; consolidation was the point of the 2026-06-12 doc.
- **Why not chosen:** the coupling IS the bug.

## Technical Considerations

### Dependencies
- New: serde + serde_yaml (config), serde_json only if Phase 0 picks it for
  package.json writes. Today's deps: clap, dirs, env_logger, eyre, log, semver,
  tempfile, toml_edit -- no serde anywhere yet; this is a real addition, added via
  `cargo add`.
- External at runtime: git, gh (existing), uv (only when uv.lock present), npm (only
  when package-lock.json present). Absent tool + present lockfile = loud error.

### Performance
- Gate probe = 2 gh API calls with retry (existing, unchanged). Lock syncs shell out
  to cargo/uv/npm -- bounded by those tools. Nothing hot.

### Security
- `install` executes a repo-committed command: same trust model as `.otto.yml` (you
  already run that repo's build tooling). No new credential custody: gh tokens stay in
  `~/.config/github/tokens/{org}` with ambient fallback.
- Fail closed everywhere a push is possible: Unknown gates refuse, dirty trees refuse,
  diverged default refuses.

### Testing Strategy
- Existing harness carries: bare TempDir remotes for real push e2e, BUMP_GATES_PROBE
  for offline gate verdicts, inline unit tests, `rule_*` version matrix.
- New: fake `gh` on PATH (or BUMP_PR_PROBE env seam, matching the gates precedent) for
  PR-create paths; per-language fixtures (uv, npm, poetry, dynamic-version, dual-manifest).
- Tests must bite: the dynamic-corruption regression test is written to fail on
  today's code first (taste.md).

### Rollout Plan
- bump phases land on bump's own flow (dogfood). Ship order forced by blast radius:
  bump merged + tagged + installed FIRST, then scottidler/claude Phase 9 re-points
  skills/agent/rules and retires the script, then manifest redeploy to other machines.
- Until Phase 9 lands, the bash script keeps working (bump's stdout is unchanged
  through Phase 8).

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| package.json write mangles formatting (JSON has no toml_edit) | Med | Med | Phase 0 picks the mechanic by round-trip testing real files; worst case targeted string edit of the one version line |
| `gh pr create --fill` semantics on re-run differ from assumption | Low | Med | Phase 0 observes it; idempotent path built on the observed behavior |
| Workspace glob members (`crates/*`) silently skipped by independent-version scan (pre-existing, cargo.rs:234-236) | Low | Med | Phase 1 adds a test documenting it; fix folded into Phase 4 if any target repo uses globs |
| Skill/agent copies on other machines go stale after Phase 9 | Med | Low | manifest redeploy is an explicit Phase 9 step, not an afterthought |
| Subcommand + legacy-flag interaction surprises (clap) | Low | Med | Phase 8 success criterion pins bare-bump byte-identical behavior on the whole matrix |
| Refuse-instead-of-rescue on stranded commits adds one manual step vs the bash driver | Low | Low | refusal prints the exact commands; deliberate trade per panel consensus (fail closed beats invented branch names) |

## Open Questions

None. All seven research questions dispositioned in Resolved Decisions; the remaining
unknowns are mechanics assigned to Phase 0 (uv lock diff scope, npm lock update
behavior, gh pr create re-run semantics), each with a falsifiable success criterion.

## Addendum: Phase 0 spike results (2026-07-06)

Three unverified mechanics, proved with throwaway projects. Commands + observed
output + chosen mechanic below. Two findings changed downstream phases (the
package.json write mechanic; the gh open-PR probe).

### uv.lock diff scope (Phase 2)

Commands (throwaway uv project, `[project]` version 0.1.0 + dep `idna==3.7`):
```
uv lock                                  # Resolved 2 packages
grep -n 'version = ' uv.lock             # -> line 16: version = "0.1.0"  (root package)
sed -i 's/0.1.0/0.1.1/' pyproject.toml   # hand-bump
uv lock                                  # "Updated uv-spike v0.1.0 -> v0.1.1"
diff before after                        # -> 16c16 only: version = "0.1.0" -> "0.1.1"
uv lock --check                          # exit 0
```
Observed: uv.lock records the root project's own version at exactly ONE site.
`uv lock` after a hand-bump touches only that line; `uv lock --check` goes green.
Decision: Phase 2 runs `uv lock` when uv.lock exists; the diff is root-version-only,
so `is_version_files_only` may accept uv.lock -- guarded by the lockfile-guard
(counts only when the change came from bump's own sync on a previously-clean tree).

### package-lock.json double-write (Phase 3)

Commands (throwaway npm project, version 0.1.0 + dep `leftpad`; sandbox disabled --
npm needs registry + a writable `~/.npm` cache, blocked in-sandbox with EROFS):
```
npm install --package-lock-only          # writes package-lock.json
grep -n '"version"' package-lock.json    # line 3 (root), line 9 (packages[""]), line 15 (dep)
sed bump package.json 0.1.0 -> 0.1.1
npm install --package-lock-only
diff before after                        # -> lines 3 and 9 only: 0.1.0 -> 0.1.1; dep untouched
```
Observed: package-lock.json carries TWO root-version sites (top-level `version` and
`packages[""].version`). `npm install --package-lock-only` after a package.json bump
updates BOTH in lockstep; dependency versions untouched.
Decision: Phase 3 writes package.json version then runs `npm install --package-lock-only`.
npm binary absent + package-lock.json present = loud error.

### package.json write mechanic (Phase 3) -- DECISION CHANGED FROM "serde_json TBD"

serde_json round-trip (preserve_order + `to_string_pretty`) on 5 real files
(gmail-filter, sieve, excalidraw, inbox-zero, archon): byte-identical ONLY after
re-appending a trailing newline (serde_json drops it) AND only because all 5 are
2-space indented. serde_json's PrettyFormatter is fixed at 2-space (reformats any
tab/4-space file) and un-escapes `\uXXXX` (churn on files with escaped unicode).

Targeted string edit on a real file (sieve): replacing only the top-level version
line yields a 1-line diff, everything else byte-identical.

Robustness finding: real package.json files have MULTIPLE `"version":` keys
(ccusage=3, pagerduty-cli=2, claude-context-mode=2) -- nested versions exist, so
first-match is unsafe.

DECISION: READ with serde_json (preserve_order) for the authoritative top-level
version (`value["version"]`, unambiguous). WRITE via targeted string edit of the
SHALLOWEST-indent `"version": "..."` line, cross-checked against the parsed old value
(bail loud on disagreement). Byte-exact like toml_edit gives Cargo.toml; zero
indent/newline/unicode churn. serde_json is added for read + validation only, never
the write. This makes the risk table's worst case ("targeted string edit") the chosen
case.

### gh open-PR probe + create (Phase 6) -- PROBE CHANGED FROM `gh pr view`

Commands (read-only, home token, against scottidler/bump which has merged PR #1 on
branch `independent-workspace-members`):
```
gh pr view no-such-branch                                   # exit 1, "no pull requests found"
gh pr list --head no-such-branch --json number              # exit 0, []
gh pr view independent-workspace-members --json state       # exit 0, state=MERGED
gh pr list --head independent-workspace-members --state open --json number   # exit 0, []
```
KEY FINDING: `gh pr view <branch>` returns exit 0 for a MERGED/closed PR -- it
conflates merged with open, so it is NOT a reliable "is there an OPEN PR" probe. A
reused branch name (prior merged PR) would falsely read as "exists" and skip create.

DECISION (supersedes the API Design table's "`gh pr view` existence check FIRST"):
the open-PR probe is `gh pr list --head <branch> --state open --json number` -- exit 0
in all cases, non-empty array = open PR (skip create), empty = create. This is the
seam the view-then-create path and the fake-gh test shim are built on.

`gh pr create --fill` on an existing OPEN PR: known gh behavior = errors (exit 1,
"a pull request for branch ... already exists"). NOT live-observed here -- it needs a
real open PR (an outward-facing side effect) for a behavior that is well-established
and, behind the open-PR probe above, only a race backstop. The gated-flow fake-gh
shim encodes it; code never calls create unless the open-PR probe returns empty.

## References

- Research brief: design-research agent run, 2026-07-06 (this session)
- `docs/design/2026-06-12-gated-repo-tagging.md` -- gates design; wrapper-script
  alternative rejected there
- `docs/design/2026-07-05-independent-workspace-members.md` -- skip-member contract
- `~/HALL-OF-SHAME.md` -- the failure catalog this design exists to end
- `~/repos/scottidler/claude/HOME/.claude/bin/release` -- the bash driver being absorbed
- rules/git.md -- tag/push invariants enforced in code by this design
