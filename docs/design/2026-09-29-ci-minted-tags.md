# Design Document: CI-Minted Tags (`tags: ci`)

**Author:** Scott Idler (drafted with Claude)
**Date:** 2026-09-29
**Status:** Draft
**Review Passes Completed:** 5/5 (log in Addendum C)

## Summary

Every `v*` tag in the fleet is created on a workstation today, by one function in `bump`
or by hand, and nothing server-side stops a wrong one. This doc moves tag creation into a
GitHub Actions workflow that runs `bump mint` under a GitHub App token, and puts a GitHub
ruleset on `refs/tags/v*` so that App is the only identity that can create a release tag.
`bump release` and `bump finish` stay the two operator verbs; they stop tagging and instead
wait for the minted tag, then install and probe as before. The invariant does not change:
the tag equals the root `Cargo.toml` version. What changes is who is physically able to
create the ref.

## Problem Statement

### Background

- The fleet invariant: one flat `v*` tag per repo, equal to the root manifest version
  (`[workspace.package].version` or `[package].version`). 67 checked-out Rust repos carry
  it: 16 tatari-tv, 1 otto-rs, 50 scottidler (enumeration in the Addendum). 33 have a
  tag-triggered workflow; marquee promotes prod on the tag.
- Every tag is created from the workstation. In `bump` it is one seam, `gate_tag_and_push`
  (`src/release/tag.rs:51-142`), reached by `bump release` on ungated repos
  (`src/release.rs:946`, `:962-1020`) and `bump finish` (`src/release/finish.rs:154`), plus
  `--tag-only` (`src/main.rs:495`) and three legacy sites in plain `bump`
  (`src/main.rs:692,739,769`).
- The record (`~/HALL-OF-SHAME.md`): roughly 50 tagging failures in four months. Orphaned
  tags, tags on the wrong sha after a squash-merge, forgotten bumps, hand-edited versions,
  `git push --tags`, two version numbers burned on one release, `-m` when patch was ordered,
  and today, 2026-09-29, marquee treated as ungated because GitHub reports `main`
  unprotected while the team convention is PR-only.
- Prose did not hold: 19 entries were written after the file was compiled and every one
  broke a rule already in it. The one mechanical guard, the 2026-07-10 hook hardening
  against bump-only branches, has had zero recurrences.
- Server-side there is nothing. Live on 2026-09-29:
  `repos/tatari-tv/marquee/rulesets?includes_parents=true` -> `[]`;
  `branches/main/protection` -> 404 `Branch not protected`; same shape on persona-cli,
  scottidler/bump, otto, claude. The tagger of marquee `v1.22.1` is `Scott A. Idler`.
- Two earlier rulings in this repo rejected CI-minted tags:
  `docs/design/2026-07-06-release-verbs-and-language-adapters.md:398-406` ("bump exists,
  the fleet is local-first, and the agent needs one deterministic verb -- not a CI
  dependency") and `docs/design/2026-09-26-one-release-command.md:335-339` ("unchanged
  ruling"). This doc reverses them, see Resolved Decisions.
- Research fan-out, 2026-09-29, five slices (official docs, engineering blogs, GitHub
  repos and issues, Hacker News, conference talks). Consensus: prose is advisory; the tool
  call or the remote is the enforcement point; the only mechanism that removes tag creation
  from the operator entirely is a GitHub ruleset whose sole bypass actor is a CI identity.
  Sources in the Addendum.

### Problem

An agent (or a human) with push rights can create a `v*` tag at any sha, from any branch,
at any time. Every guard that exists runs on the agent's side of the wire and depends on
the agent using the guarded tool. The fix has to be a guard the agent cannot route around:
one that lives on GitHub, with a bypass list containing no human and no agent.

### Goals

Traceable to Scott, 2026-09-29, this session:

- "what will it take to PREVENT you from fucking up something as simple as tagging?": a
  release tag can only be created by CI, on the default-branch tip, equal to the manifest
  version, after green CI. A workstation push of `v*` is rejected by GitHub.
- "this sounds more like machinery using gha than any change to bump": the load-bearing
  layer is GitHub-side (ruleset, App, workflow). `bump` changes only where it must: it
  stops creating the ref and learns to wait for it.
- "this is not just marquee. many of my rust cli, rest, mcp, etc projects have this same
  problem": fleet-wide, applied by config, marquee first because it deploys prod on the tag.
- The gate misdetection class dies: GitHub state matches convention, so `bump --gates`
  is truthful with no declared override.
- `bump release` / `bump finish` remain the only two operator commands. The install and
  probe half stays on the workstation (preserves the 2026-07-06 reasoning).

### Non-Goals

- **Bump level (patch | minor | major) selection.** The version commit is still authored
  by `bump release [-m|-M]` and rides a PR on gated repos. Parked; revisit if a wrong-level
  version commit ever merges (the 2026-09-27 `-m` incident was local and caught).
- **GitHub immutable releases.** Makes a wrong tag permanent. Revisit only after the
  ruleset is `active` fleet-wide.
- **Signed tags.** Not requested.
- **Per-crate tags, crates.io publishing, release-plz adoption.** `rules/git.md:14`
  forbids multi-scheme tags; the fleet publishes binaries and images, not crates.
- **Reconciling `tatari-tv/github-setup-rs` with the live Python `github-setup`.** The
  Rust port's config is stale (no `marquee` entry) and no CI applies it. Handed to that
  repo's owner; this doc uses the tool that is live.
- **Changing merge policy on scottidler personal repos.** Main stays ungated there; only
  the tag ruleset applies.
- **Renaming or moving existing tags.** Never (`rules/git.md:9-11`).

## Proposed Solution

### Overview

Four layers, outermost first. Each one alone has a hole; together the agent has no path
to a wrong tag.

| Layer | Where | What it guarantees | Fails how |
|---|---|---|---|
| Rulesets | GitHub, `refs/tags/v*`: `create` with the App as sole bypass, `immutable` with none | No human or agent token can create a `v*` tag; nobody, App included, deletes or moves one | Push rejected with the ruleset name |
| Workflow | Reusable `mint.yml` in `scottidler/bump`, caller in each repo, key in a `main`-only environment | Tag created only on the default-branch tip, after CI, equal to the manifest, by the App; no `v*` tag without a merge to `main` | Run fails loudly; nothing minted |
| `bump` | `bump.yml` `tags: ci`; `bump mint` (CI verb); `release`/`finish` wait instead of tag | The local tag path is refused on a `tags: ci` repo; the verbs still finish the release (install, probe) | Refusal names the workflow |
| Hook | `git-release-guard.sh` | An agent typing raw `git tag` / `git push origin v*` on a `tags: ci` repo is denied before the shell runs | Deny reason quotes the rule |

Main-branch gating rides the same change: tatari-tv repos whose convention is PR-only get
classic branch protection through the live `github-setup` config, so `github::detect`
sees what the team means. No `gate:` override in `bump.yml` (Alternatives, 1).

### Architecture

Gated repo (tatari-tv), the marquee shape:

```
agent: bump release            -> version commit on the feature branch, push, PR (unchanged)
human: merge PR                -> squash commit S on main
GitHub: ci.yml runs on S       -> deploys test (unchanged)
GitHub: tag.yml (workflow_run on ci success, branch main)
        -> uses scottidler/bump/.github/workflows/mint.yml@vX.Y.Z
        -> App token (contents: write) -> cargo install bump @ workflow ref
        -> bump mint --sha S
             ladder: S == origin/main tip, manifest(S) -> vN, vN absent on origin
             wait_for_green(S) excluding this run
             git tag -a vN S ; git push origin vN         (App identity)
GitHub: ruleset admits the push (Integration bypass)
GitHub: release.yml fires on vN  -> CLI release, promote prod (unchanged)
agent: bump finish             -> ff main, ladder, WAIT for vN at S on origin, install
agent: sdv probe               -> proves prod serves vN @ S (unchanged)
```

Ungated repo (scottidler), direct-to-main shape:

```
agent: bump release            -> version commit on main, push main (no tag)
GitHub: tag.yml (on push main, or workflow_run on ci if the repo has one)
        -> mint.yml -> bump mint --sha S -> vN pushed by the personal App
GitHub: per-repo ruleset admits it
agent: bump release (same run) -> waits for vN at S on origin, installs
```

Two merges in quick succession: the run for the older sha A finds `origin/main != A`,
prints "superseded by B, nothing to mint", exits 0. The run for B mints. If A carried
version X and B carries Y, vX is never created: nothing was published under it, so
nothing is burned. This is the 2026-09-26 ruling ("the tag goes on the newest commit on
the default branch at tagging time", `2026-09-26-one-release-command.md:318`) applied by
construction.

A merge whose CI fails mints nothing (`workflow_run` conclusion is not `success`, and
`mint`'s own `wait_for_green` refuses on the `push` variant). The fix merge, carrying the
same version, mints it. No version is burned by a red build: the double-tap class
(otto v2.0.4/v2.0.5) has no path.

### Components

**GitHub App: `tag-minter`** (name is Scott's call, Open Question 3). Permissions
`contents: write` (create the ref), `checks: read` and `statuses: read` (the CI gate reads
check runs and commit statuses on private repos), `actions: read` (the run-id exclusion
reads its own run), `metadata: read`. Nothing else. Two registrations because there are
two custody models:

- tatari-tv org App, installed on selected repos = the `tags: ci` set. Private key and
  client id live in a GitHub Environment named `tag-minter` in each repo, with a
  deployment branch policy of `main` only, so a job can read them only when it runs on a
  ref that is already on `main`. A PR workflow cannot reach them. The org-secret shortcut
  (one place, every workflow in the repo can read it) is rejected: it hands the minting
  key to any job any writer merges. Environment creation is one `gh api` call per repo in
  the sweep.
- scottidler personal App, installed on the in-scope personal repos and on `otto-rs`
  (Scott's own org, one repo; it rides the home persona, not the work one). Same
  environment shape, set per repo by `gh secret set --env tag-minter` in the sweep
  (Phase 9). One key per persona, never one key across both (`rules/secrets.md`).

What the credential placement guarantees, stated exactly: no `v*` tag exists without a
merge to `main`. The ruleset closes every workstation and every PR-time path. A workflow
merged to `main` that misuses the environment could still push a tag; that path is a
reviewed diff in the repo's history, not an agent's shell. This doc does not claim more.

Least-privilege delta against reuse: `tatari-skills-versioning` (App 3522444) has
`contents: write` plus `pull_requests: write` and is installed on selected skill repos;
`tatari-deployments` has `contents: read` and cannot tag. Reusing the former widens a
skills-named App onto 16 unrelated repos and carries one extra write permission. A new App
named for what it does costs one registration. `rules/taste.md`: names tell the truth.

**Two rulesets, because a bypass actor bypasses a whole ruleset.** A single ruleset with
`creation` plus `deletion`/`update` and the App on its bypass list would let the App
delete or move tags too. So:

- `release-tags-create`: target `tag`, `ref_name.include: ["refs/tags/v*"]`, rule
  `creation`, `bypass_actors: [{actor_type: Integration, actor_id: <tag-minter>,
  bypass_mode: always}]`. No roles, no teams, no users.
- `release-tags-immutable`: same target and pattern, rules `deletion`, `update`,
  `non_fast_forward`, empty bypass list. Nobody, the App included, moves or deletes a
  release tag (`rules/git.md:9-13`, now enforced server-side).

Both `active` after an `evaluate` window where the plan offers one. Two placements:

- tatari-tv: the pair as org-level rulesets conditioned on a new org custom property
  `tags` with value `ci` (`conditions.repository_property`), `values_editable_by:
  org_actors` so a repo cannot opt itself out. This copies the mechanism already in
  force: org ruleset 126206 gates `main` on `managed=true` repos by custom property. Per-
  repo opt-in is one line in the live `tatari-tv/github-setup/github-setup.yml`. The
  Python tool writes only `managed` today (`GithubRepo`, `github_setup/repo.py:11-23`,
  has no `tags` field; the apply path hardcodes `{'managed': ...}`,
  `github_setup.py:469-471`), so Phase 6 carries a small PR to that repo: a `tags` field
  on the model, the diff, and the apply. Creating the property and the org rulesets needs
  an org admin (Scott is one; the CLI token needs `admin:org`, observed:
  `orgs/tatari-tv/rulesets` -> 404 "needs the admin:org scope").
- scottidler and otto-rs: no custom properties, so the same pair as per-repo rulesets,
  applied by a `gh api` sweep (Resolved Decisions).

**Main protection, tatari-tv.** Classic protection on `main`, PR required, through the
live `github-setup` config (`protections:` block, already supported, per-repo). Which
repos: every tatari-tv repo in scope (Open Question 2). marquee stays `managed: false`
(it opted out of org ruleset 126206's required-workflows gate); protection lands on it
directly. After this, `bump --gates` on marquee reports gated with no override.

**Reusable workflow `mint.yml`** in `scottidler/bump/.github/workflows/`, `on:
workflow_call`. Inputs: `sha` (required). No declared secrets: the job reads the caller
repo's `tag-minter` environment secrets itself (see the secrets paragraph below).
Steps: the job declares `environment: tag-minter` and reads the two environment secrets;
`actions/create-github-app-token` scoped to the calling repo (harvest
`tatari-tv/github-actions/.github/workflows/platform-promote-prod.yml:78-90`), exported
as `GH_TOKEN` for the job so `bump`'s `gh api` calls carry it; checkout `sha` with
`fetch-depth: 0` and the App token; `git config user.name "tag-minter[bot]"` and
`user.email "<bot-user-id>+tag-minter[bot]@users.noreply.github.com"`, where the id is
the bot USER id from `gh api /users/tag-minter%5Bbot%5D --jq .id`, not the App id (per
the `actions/create-github-app-token` README), so the annotated tag's tagger is the App; `cargo install --locked --git https://github.com/${{
job.workflow_repository }} --rev ${{ job.workflow_sha }}` with a cargo cache;
`bump mint --sha ${{ inputs.sha }}`. `job.workflow_sha` is "The commit SHA of the workflow
file that defines the current job" (docs.github.com contexts reference; the `github`
context in a called workflow is the caller's, and `github.job_workflow_sha` does not
exist). `concurrency: { group: mint-${{ github.repository }}, cancel-in-progress: false }`
(harvest `tatari-skills/.github/workflows/auto-version.yaml:22-24`). The workflow builds
the same `bump` revision the caller pinned, so the ladder in CI and the ladder on the
workstation are one implementation.

`concurrency` with `cancel-in-progress: false` keeps one running and one pending run per
repo; a third arrival replaces the pending one. Arrival order is CI-completion order, not
commit order, so the displaced run can be the tip's own (panel round 2: main goes
A -> B -> C, CI(C) finishes first, `mint(C)` is pending, `mint(B)` replaces it). The
displaced-tip case is closed inside `mint`, not by the queue: a run that finds itself
superseded follows the tip when the manifest there carries the same version (the
moving-tip rule, gate and tag the tip), and when the version differs it re-dispatches
`tag.yml` for the tip (`gh workflow run tag.yml -f sha=<tip>` under `GITHUB_TOKEN` with
`permissions: actions: write`; `workflow_dispatch` always creates a run, whichever token
sends it) and exits 0 with `superseded by <tip>, re-dispatched`. Either way the tip gets
a run that reaches the tag step.

Secrets in a called workflow: `on: workflow_call` has no `environment` keyword, and the
reusable-workflows doc states "Environment secrets cannot be passed from the caller
workflow ... If you include `environment` in the reusable workflow at the job level, the
environment secret will be used". So the caller passes `secrets: inherit` with no
mapping, and the `mint` job in `mint.yml` declares `environment: tag-minter` and reads
`secrets.TAG_MINTER_CLIENT_ID` / `secrets.TAG_MINTER_PRIVATE_KEY` itself; the environment
resolves in the caller's repo. Phase 0(e) proves this cross-repo path with no repo-level
secret present.

`scottidler/bump` is public (observed: `visibility: PUBLIC`), so tatari-tv callers can
`uses:` it if the org Actions policy allows outside reusable workflows. Phase 0 proves it;
fallback is a one-job wrapper in `tatari-tv/github-actions` that `uses:` the same file.

**Caller `tag.yml`** in each repo, two variants. Both also carry `workflow_dispatch`
(optional `sha`, default the tip) as the retry path after a failed run and as the fleet
probe: a dispatch on an already-tagged tip logs `already minted` or `no new version` and
proves the wiring, the environment and the App without creating anything.

```yaml
# repos with a CI workflow (name: ci)
on:
  workflow_run:
    workflows: [ci]
    types: [completed]
    branches: [main]
  workflow_dispatch:
    inputs: { sha: { required: false } }
jobs:
  mint:
    # workflow_run filters on branch NAME, so a fork PR from a branch called `main`
    # would fire this with the key in reach; the head_repository check closes it.
    if: >-
      github.event_name == 'workflow_dispatch' ||
      (github.event.workflow_run.conclusion == 'success' &&
       github.event.workflow_run.head_repository.full_name == github.repository)
    permissions: { contents: read, actions: write }
    uses: scottidler/bump/.github/workflows/mint.yml@vX.Y.Z
    with:
      sha: ${{ inputs.sha || github.event.workflow_run.head_sha || github.sha }}
    secrets: inherit
```

```yaml
# repos with no CI workflow
on:
  push:
    branches: [main]
  workflow_dispatch:
    inputs: { sha: { required: false } }
jobs:
  mint:
    permissions: { contents: read, actions: write }
    uses: scottidler/bump/.github/workflows/mint.yml@vX.Y.Z
    with:
      sha: ${{ inputs.sha || github.sha }}
    secrets: inherit
```

The `push` variant is for repos where NO remaining workflow runs on a push to the
default branch. A repo whose only other workflow is tag-triggered (`scottidler/scan`,
`release.yaml` on `push: tags`) or PR-only also commits `ci: none` in `bump.yml`, because
`mint`'s CI gate would otherwise see a workflows tree, zero checks, and refuse
(`ci.rs:114`). The sweep decides both from the parsed `on:` of every workflow in the repo.

Pin is an exact `bump` tag. tatari-renovate updates it; personal repos re-pin in a sweep
when `mint` changes.

**`bump.yml` fact `tags: ci`.** Committed, read at the sha (`config::load_at`,
`src/config.rs:83-89`) for every decision that suppresses a safety step, same reasoning as
`ci: none` (`src/config.rs:79-82`). Absent means `local`, today's behavior.

**`bump mint`** (new verb, CI only). Refuses unless `GITHUB_ACTIONS=true`. Contract in API
Design.

**`bump finish` under `tags: ci`.** Steps 1-6 unchanged (`src/release/finish.rs:65-135`:
worktree resolve, tracked-changes refusal, `reach_merged_tip`, `config::load`,
`tag_ladder`). Step 7 (`gate_tag_and_push` at `:154`) becomes `wait_for_minted_tag`,
which keeps the moving-tip rule that `gate_tag_and_push` enforces today
(`src/release/tag.rs:73-101`): poll `git::remote_tag_commit` (`src/git.rs:476-511`) for
`vN`; on each poll also fetch `origin/<default>`. If the tip moved to a sha whose
manifest still carries `N`, follow it (print the move, as today) and keep waiting for
`vN` there; if the manifest at the new tip carries another version, refuse (as today).
Accept `vN` when it is on origin at the followed tip, OR at an ancestor of the followed
tip whose manifest carries `N` (the release happened, then an unbumped commit landed
before `finish` looked; `release-tags-immutable` means the tag can never move to the
tip, so waiting for it there would never end). Install from the tag's sha. `vN` at a
sha that is neither refuses. A generic repo (no manifest) refuses before the wait, as
`finish.rs:111-118` does today. Timeout refuses and names the repo's `tag.yml` Actions
URL. Timing lives on the `Ci` port (`src/release/ci.rs:31-39`) so
tests never sleep. `TagState::RemoteAtHead` (`:173-181`) already models "released,
install"; `LocalAtHead` becomes a refusal (a local tag must not exist on a `tags: ci`
repo). Step 8 install runs from the accepted sha.

**`bump release` under `tags: ci`, ungated.** Version commit, `push_branch`,
`confirm_on_origin` (`src/release.rs:1219-1229`), then the same wait, then install. Never
`push_tag`. Gated `bump release` is unchanged (it never tagged, `src/release.rs:17-22`).

**Refusals under `tags: ci`:** `bump --tag-only`, the three legacy tag sites in plain
`bump`, and `bump mint` outside Actions. Each names the one next command.

**`bump --gates`** prints the declared `tags:` mode and, informationally, whether a
tag-target ruleset is live (`repos/{slug}/rulesets?includes_parents=true`, filter
`target == "tag"`). Report only; the decision input is the committed fact. Divergence in
either direction fails loud: `local` with a ruleset -> the push is rejected by GitHub;
`ci` with no workflow -> `finish` times out naming the workflow.

**Hook `git-release-guard.sh`** (scottidler/claude). On a statement whose worktree's
COMMITTED `bump.yml` (`git show HEAD:bump.yml`) says `tags: ci`: deny tag creation (`git
tag <name>` in any form, allowed today on main, `:311-337`), deny `git push <remote>
v*` and `refs/tags/v*`, deny `bump --tag-only` and `bump mint`. `bump release` / `bump
finish` stay exempt (`:427-429`). Self-test rows added to `git-release-guard-test.sh`.

**Agent and skills** (scottidler/claude): `release-driver.md:60-78` verb contract gains
the `tags: ci` outcome ("finish waits for the minted tag"), `:161-167` tag verification
adds "tagger is the App"; `skills/bump/SKILL.md:18-32,56-62` and
`skills/shipit/SKILL.md:42-59` name the mode. `settings.json` allow rules `Bash(bump
--tag-only:*)` (`:92`), `Bash(git push origin v*)` (`:93`), `Bash(git tag:*)` (`:112`) are
narrowed; Scott applies that file (auto mode denies agents editing it, precedent
2026-09-26 Phase 8).

**marquee docs.** `docs/deploy.md:125-126` ("the version-increment tool creates it on
`main`, as always; CI never tags") and `release.yml:15-17` ("tags are owned by `bump`,
never CI") are rewritten to the new truth.

### Data Model

`bump.yml`, complete schema after this doc (kebab-case, `deny_unknown_fields`):

```yaml
skip-members: [claude-pricing]   # existing
install: cargo install --path clyde   # existing
ci: none                          # existing, single-variant enum
tags: ci                          # NEW, enum {local, ci}; absent == local
```

Org custom property (tatari-tv): `tags`, `single_select`, allowed values `["ci"]`. Absent
means no ruleset.

Ruleset bodies (REST `POST /repos/{o}/{r}/rulesets` for personal repos; `POST
/orgs/tatari-tv/rulesets` for the org pair, plus `conditions.repository_property`):

```json
{
  "name": "release-tags-create",
  "target": "tag",
  "enforcement": "active",
  "conditions": { "ref_name": { "include": ["refs/tags/v*"], "exclude": [] } },
  "rules": [ {"type":"creation"} ],
  "bypass_actors": [ { "actor_type": "Integration", "actor_id": 0, "bypass_mode": "always" } ]
}
{
  "name": "release-tags-immutable",
  "target": "tag",
  "enforcement": "active",
  "conditions": { "ref_name": { "include": ["refs/tags/v*"], "exclude": [] } },
  "rules": [ {"type":"deletion"}, {"type":"update"}, {"type":"non_fast_forward"} ],
  "bypass_actors": []
}
```

Environment `tag-minter` per repo: `deployment_branch_policy: { protected_branches:
false, custom_branch_policies: true }` with the single policy `main`; secrets
`TAG_MINTER_CLIENT_ID`, `TAG_MINTER_PRIVATE_KEY` set on the environment, not the repo.

### API Design

`bump mint --sha <sha>` (CI only):

- Preconditions, each a refusal with exit 1 and no side effect: `GITHUB_ACTIONS != true`;
  `GH_TOKEN` unset; tracked changes; `HEAD != <sha>`; committed `bump.yml` at `<sha>`
  lacks `tags: ci`.
- Generic repo (no manifest at `<sha>`) -> exit 1, as every other verb.
- Ladder (reuses `tag_ladder`, `src/main.rs:393-458`, with the sha explicit): fresh fetch;
  manifest at `<sha>` -> `vN`. If `origin/<default>` tip != `<sha>`: when the manifest at
  the tip also carries `N`, follow the tip (`sha := tip`, the moving-tip rule of
  `tag.rs:73-101`) and continue; when it carries another version, run `gh workflow run
  tag.yml -f sha=<tip>` and exit 0 with `superseded by <tip>, re-dispatched`.
- Remote `vN` at `<sha>` -> `already minted`, exit 0 (idempotent re-run). Remote `vN` at
  an ancestor of `<sha>` AND no `v*` tag reachable from `<sha>` carries a version greater
  than `N` -> `no new version` (a merge that did not bump), exit 0. Any other placement
  of `vN`, including `vN` at an ancestor with a higher tag between it and `<sha>` (a
  reverted manifest), -> exit 1, `vN exists at <other>; the version was reused`.
- CI gate: `wait_for_green(<sha>)` (`src/release/ci.rs:66-148`) with two changes. Check
  runs whose `details_url` contains `/actions/runs/$GITHUB_RUN_ID/` are excluded, which
  needs `check_runs_from_json` (`src/github.rs:574`) to keep per-run identity instead of
  reducing to counts. And the zero-checks decision (`ci.rs:114`, "has a workflows tree ->
  refuse after the appear window") reads the tree at `<sha>` minus the caller file named
  by `GITHUB_WORKFLOW_REF` (the caller's file; in a called workflow the `github` context
  and its env mirror belong to the caller). A repo whose only workflow is `tag.yml`
  proceeds with the printed no-CI notice. A repo with other workflows that never run on
  a push to the default branch (tag-only, PR-only) still refuses unless its committed
  `bump.yml` says `ci: none`, exactly as today; the sweep writes that fact from the
  parsed triggers. Red -> exit 1, no tag.
- The two re-verifies from `2026-09-26-one-release-command.md:193`, kept in order: fresh
  fetch, tip must equal `<sha>`, else exit 1 and nothing is created; `git tag -a vN -m
  "Release vN" <sha>` (`git::create_tag`); fresh fetch again, tip must still equal
  `<sha>`, else exit 1 (the local tag dies with the runner); `git push --no-follow-tags
  origin vN` (`git::push_tag`, by name).
- Output, one line per step, last line `minted vN at <sha>` | `already minted` | `no new
  version` | `superseded by <tip>, re-dispatched`. The job needs `permissions:
  actions: write` (the caller grants it) for the re-dispatch only; the App token is not
  used for it.

Budgets, all on the `Ci` port so tests inject them: poll interval, the port default
(`src/release/ci.rs:31-34`); appear window 120s (`CI_APPEAR_WINDOW`, `ci.rs:20`); CI
timeout 1800s (`DEFAULT_CI_TIMEOUT`, `ci.rs:23`); App installation token 1h (GitHub's
limit), which caps a single `mint` run. `bump finish` / `bump release` wait on the same
budgets; the refusal text on timeout: `vN not on origin after <t>; see
https://github.com/<slug>/actions/workflows/tag.yml`.

### Implementation Plan

Ten phases, 0 through 9. Three are operator steps Scott runs (5, 6, 9a); the rest are
code or sweeps. Ship order is forced by dependency: bump (verb, facts, workflow) -> claude (hook, agent,
skills) -> GitHub operator steps (App, property, ruleset, secrets, protection) -> marquee ->
tatari-tv sweep -> scottidler sweep. Phases 5, 6 and 9a are operator steps: Scott runs
them, the doc records the exact commands and the observed outputs.

#### Phase 0: Prove the GitHub mechanics on scratch repos
**Model:** opus. Zero code. One scratch repo in tatari-tv, one in scottidler.
- (a) Hand-create the tag ruleset (target tag, `creation`, bypass = a scratch App). From the
  workstation as Scott (org admin): `git push origin v9.9.9`. Expect rejection naming the
  ruleset. This answers whether admins are blocked (they are, unless listed; prove it).
- (b) A workflow under the App token pushes `v9.9.8`; a second `on: push: tags` workflow
  must run. Repeat under `GITHUB_TOKEN`: it must not run.
- (c) A tatari-tv scratch caller `uses: scottidler/<scratch>/.github/workflows/x.yml@main`.
  Expect a run, or the org-policy error verbatim.
- (d) `${{ job.workflow_sha }}` and `${{ job.workflow_repository }}` inside the called
  workflow: assert non-empty and equal to the pinned ref's sha and `scottidler/<scratch>`;
  `cargo install --git --rev` of bump at that sha completes, record wall time.
- (e) The exact secret path: a tatari-tv scratch caller with `secrets: inherit` and NO
  repo-level `TAG_MINTER_*` secrets calls a scottidler scratch reusable whose job
  declares `environment: tag-minter` and echoes the length of `secrets.TAG_MINTER_CLIENT_ID`.
  Non-zero when run from `main`; the job is refused by the branch policy when dispatched
  from another branch.
- **Success criteria:** five observed outputs pasted into this doc under this phase; (a)
  rejected, (b) fires under App and not under `GITHUB_TOKEN`, (c) runs or names the
  policy, (d) both values asserted and build time recorded, (e) secret readable from
  `main` only.

#### Phase 1: `bump.yml` `tags:` fact
**Model:** sonnet.
- `TagsDeclaration { Local, Ci }` in `src/config.rs` beside `CiDeclaration`; `tags:
  Option<TagsDeclaration>`; `deny_unknown_fields` kept.
- Read via `load_at(sha)` wherever it suppresses a step; `load` only for `--gates` display.
- `bump --gates` prints `Tags: ci (declared)` | `Tags: local`, plus the informational
  ruleset probe, and records both gate endpoints it read (`branches/<b>/protection` and
  `rules/branches/<b>`) with their status codes.
- Probe hardening: `BUMP_GATES_PROBE` (`src/github.rs:52`) is honored only in debug
  builds (`cfg!(debug_assertions)`), so a release binary cannot be steered by an env var;
  tests cover a classic-protection 404 with a live ruleset, and a stale `origin/HEAD`.
- **Success criteria:** `bump release -n` in a repo whose committed `bump.yml` says `tags:
  ci` no longer errors (today: `unknown field 'tags', expected one of 'skip-members',
  'install', 'ci'`); `tags: bogus` is a loud serde error naming the key; a unit test
  `tags_ci_read_from_sha_not_worktree` proves an untracked `bump.yml` cannot switch the
  mode; `BUMP_GATES_PROBE=ungated bump --gates` on the release binary still probes.

#### Phase 2: `bump mint`
**Model:** opus.
- New verb per API Design; reuses `tag_ladder`, `wait_for_green`, `create_tag`,
  `push_tag`. `check_runs_from_json` keeps per-run `details_url` (fixtures with and
  without the own-run entry); the zero-checks decision subtracts the caller file.
- Tests on bare-origin fixtures (`src/release/tests.rs:401-423,543-555`): mint at tip
  mints once; second run is `already minted`; unbumped merge over an older `vN` is `no
  new version`; reverted manifest (v1 tagged, v2 tagged, tip back to v1) -> exit 1
  `version reused`; tip moved with the same version -> follows and tags the tip; tip
  moved with a different version -> `superseded ... re-dispatched` and the recorded
  dispatcher saw the tip's sha; out-of-order CI completion (B's run arrives after C's)
  still leaves C tagged; version reused at a non-ancestor -> exit 1; red CI -> no tag;
  own run excluded from the gate; tree with only `tag.yml` proceeds; tag-only and
  PR-only trees refuse without `ci: none` and proceed with it; main CI not yet
  registered waits within the appear window; generic repo refuses; `GITHUB_ACTIONS`
  unset -> refusal. Break the code: remove the pre-create fresh fetch and
  `mint_refuses_when_tip_moved_before_create` must fail.
- **Success criteria:** the named tests pass; `otto ci` green; `bump mint` from a
  workstation shell exits 1 with no tag.

#### Phase 3: `bump finish` and `bump release` wait instead of tag
**Model:** opus.
- `finish.rs:154` and `release.rs:946,962-1020`: under `tags: ci`, replace
  `gate_tag_and_push` with `wait_for_minted_tag(sha)`; `LocalAtHead` refuses; install
  unchanged. `--tag-only` and `main.rs:692,739,769` refuse under `tags: ci`.
- Copy `LandingCi` (`src/release/tests.rs:330-360`) into a `LandingTag` double that pushes
  `vN` to the bare origin mid-wait.
- **Success criteria:** `finish_waits_for_minted_tag_then_installs` (RecordingInstaller
  ran from the accepted sha, RecordingPusher never saw `push_tag`);
  `finish_follows_moving_tip_with_same_version` (tip moves mid-wait, manifest unchanged,
  `vN` lands on the new tip, finish accepts and installs from it);
  `finish_accepts_tag_at_ancestor_after_unbumped_commit` (vN minted at A, docs-only B
  lands before finish looks, finish installs from A), in both orders of observation;
  `finish_refuses_moving_tip_with_new_version`; `finish_refuses_tag_at_other_sha`;
  `finish_timeout_names_workflow`; `tag_only_refuses_under_tags_ci`, asserted as `git
  for-each-ref refs/tags` identical before and after (today `bump --tag-only` on such a
  repo prints `Tagged v0.1.0 on merged main` and creates it).

#### Phase 4: reusable `mint.yml` and caller templates
**Model:** sonnet.
- `.github/workflows/mint.yml` in this repo per Components; `docs/` gets the two caller
  variants (markdown only); this repo's own `tag.yml` caller uses the local path
  `./.github/workflows/mint.yml` and builds `bump` at `github.sha`.
- **Success criteria:** `actionlint` clean; on this repo, one merge that changes the
  version mints exactly `v<version>` at the merge sha with tagger `tag-minter[bot]`; a
  docs-only merge mints nothing; re-running the workflow on the same sha logs `already
  minted`.

#### Phase 5: App registrations and secrets (operator)
**Model:** none, Scott.
- Register `tag-minter` in tatari-tv with the five permissions in Components, install on
  the in-scope set. Register the personal App on scottidler and install it on the
  personal set plus `otto-rs`. Per repo: environment `tag-minter`, branch policy `main`,
  the two secrets on the environment (`gh api` + `gh secret set --env`, scripted in the
  sweep).
- **Success criteria:** `gh api orgs/tatari-tv/installations` lists the App with exactly
  `contents: write, checks: read, statuses: read, actions: read, metadata: read` and
  `repository_selection: selected`; `gh api
  repos/<slug>/environments/tag-minter/deployment-branch-policies` returns exactly one
  policy, `main`.

#### Phase 6: property, org ruleset, main protection (operator)
**Model:** none, Scott, `admin:org` token.
- Create custom property `tags` (`values_editable_by: org_actors`); create the org
  ruleset pair in `evaluate` where the plan allows it; PR to `tatari-tv/github-setup`
  adding a `tags` field to `GithubRepo` (`repo.py:11-23`), its readback into the
  snapshot, its diff, and its apply beside `managed` (`github_setup.py:469-471`), with
  one difference from `managed`: an absent desired value is an explicit clear
  (`update_custom_properties({'tags': None})`, the REST unset), not a skip, so removing
  the line from the config removes the property. Test: set -> readback -> empty diff ->
  clear -> readback -> empty diff. Then `tags: ci` and the `protections:` block for
  `main` on marquee in `github-setup.yml`. The tool reconciles every three hours
  (`ci.yaml:7-8`), so the property is owned by the config from then on.
- **Success criteria:** `repos/tatari-tv/marquee/rulesets?includes_parents=true` shows two
  `target: tag` rulesets; `repos/tatari-tv/marquee/properties/values` shows `tags=ci`;
  `bump --gates` in marquee prints gated and records both endpoints; the github-setup PR's
  test covers `tags`.

#### Phase 7: hook, agent, skills (scottidler/claude)
**Model:** sonnet.
- Hook rows per Components; `release-driver.md`, `skills/bump`, `skills/shipit` updated;
  `rules/git.md:53-57` (the living rule: "tags, pushes the tag") rewritten for `tags: ci`;
  `settings.json` narrowing handed to Scott as a patch. The hook denies creation of ANY
  tag on a `tags: ci` repo, not only `v*`: `rules/git.md:14` already forbids every other
  tag scheme, so there is no legitimate non-`v*` tag to allow.
- **Success criteria:** `git-release-guard.sh --self-test` reports `pass>=279 fail=0`
  plus the new rows (today `pass=279 fail=0`); the new rows fail against the pre-change
  hook; `bump release` and `bump finish` still pass.

#### Phase 8: marquee first
**Model:** sonnet, work persona.
- PR: `tag.yml` caller (workflow_run on `ci`), `bump.yml` with `tags: ci`, `docs/deploy.md`
  and `release.yml` header rewrites. Flip the org ruleset to `active` once the PR merges
  and the first minted tag lands.
- **Success criteria:** the merge mints exactly one `v*` tag at the squash sha, tagger is
  `tag-minter[bot]` AND the `tag.yml` run for that sha logs `minted vN at <sha>` (the
  tagger field alone is editable metadata; the run log is the provenance); `release.yml`
  runs and `promote-prod` completes; `sdv probe` shows version and revision matching the
  tag; a workstation `git push origin v0.0.0-probe` on marquee is rejected naming
  `release-tags-create`.

#### Phase 9: fleet sweeps
**Model:** fable.
- 9a (operator): `tags: ci` property and `main` protection for the remaining tatari-tv
  repos in scope via `github-setup.yml`; personal App installed on the scottidler set.
- 9b (work persona): one PR per remaining tatari-tv repo: caller + `bump.yml` (`clyde`
  keeps its existing keys). Per repo, read the CI workflow's `name:` and `on:` first: a
  CI that runs on push to main gets the `workflow_run` caller naming it; a CI that runs
  on PRs only, or no CI, gets the `push` caller. `vault-for-brands-api` releases on
  `release:` events, decided separately.
- 9c (home persona): scottidler repos and otto-rs/otto: the ruleset pair, the
  environment and its two secrets, caller + `bump.yml`, applied by `bin/tag-guard-sweep.sh`
  in this repo (a `gh api` loop over a repo list; Resolved Decisions), same CI-trigger
  inspection as 9b.
- 9d (home persona): the `create-repo` and `scaffold-rust-repo` skills lay down
  `tag.yml`, `bump.yml` with `tags: ci` (and `ci: none` when the scaffold ships no CI),
  the environment, the secrets, the ruleset pair, and add the repo to the App's selected
  installation, so the fleet does not drift back.
- **Success criteria**, three checks per repo because one probe cannot cover them:
  (1) credential smoke: a `workflow_dispatch` of `tag.yml` on the tagged tip completes
  with `already minted` or `no new version` (proves environment, App and wiring; exits
  before the CI gate, so it does not prove the gate); (2) trigger correctness: the
  sweep's parsed-`on:` decision is recorded per repo (variant and whether `ci: none` was
  written), and Phase 2's trigger-class tests cover each class the sweep produced;
  (3) first release: the repo's next version bump lands as a minted tag with the App as
  tagger and the run log line, recorded when it happens. Plus: every in-scope repo has a
  committed `bump.yml` naming `tags: ci` (today: 1 of 67 has a `bump.yml` at all, none
  names `tags`); no repo has a local `v*` tag ahead of origin; a repo created with
  `create-repo` after 9d passes check (1) on its first tagged release, not before (a
  fresh scaffold has no tag to be idempotent against).

## Acceptance Criteria

- [ ] A workstation push of a `v*` tag to tatari-tv/marquee is rejected by GitHub with a
  message naming `release-tags-create`.
  `Observed on main (2026-09-29):` not run against marquee (it would create a tag); the
  Phase 0 scratch repo carries this proof. Live state: `repos/tatari-tv/marquee/rulesets?includes_parents=true` -> `[]`.
- [ ] The newest `v*` tag on every in-scope tatari-tv repo has tagger `tag-minter[bot]`
  and a `tag.yml` run on its target sha whose log ends `minted vN at <sha>`.
  `Observed on main (2026-09-29):` marquee `v1.22.1` tagger `Scott A. Idler <scott.idler@tatari.tv>`, target `849bb96`; no `tag.yml` exists.
- [ ] `bump release -n` in a repo whose committed `bump.yml` says `tags: ci` runs the
  dry run and prints `Tags: ci`.
  `Observed on main (2026-09-29):` `Error: failed to parse .../bump.yml: unknown field 'tags', expected one of 'skip-members', 'install', 'ci'`.
- [ ] `bump --tag-only` in that repo exits non-zero and `git for-each-ref refs/tags` is
  identical before and after.
  `Observed on main (2026-09-29):` prints `Tagged v0.1.0 on merged main (8778d35b63ec)`; the ref set gains `refs/tags/v0.1.0`.
- [ ] `git-release-guard.sh --self-test` passes with the `tags: ci` rows, and those rows
  fail against the pre-change hook.
  `Observed on main (2026-09-29):` `pass=279 fail=0`.
- [ ] `bump --gates` on marquee reports gated with no override set, and records both
  endpoints it read.
  `Observed on main (2026-09-29):` `Gates: none (ungated)`; `branches/main/protection` -> 404 `Branch not protected`; `rules/branches/main` -> `[]`.

## Resolved Decisions

- 2026-09-29, this doc supersedes `2026-07-06` Alternative 2 and `2026-09-26` Alternative 3
  (CI-minted tags rejected). What survives from those rulings: the two operator verbs, the
  workstation install/probe half, one implementation of the ladder (the workflow runs
  `bump`). What changes: the ref is created by the App. Reason: 19 failures since the
  shame file was compiled, all on the operator side; the server-side rule is the only
  layer the agent cannot route around. Scott's confirmation of the reversal is Open
  Question 1 until he gives it.
- 2026-09-29, author: no `gate:` override in `bump.yml`. The misdetection is fixed by
  making GitHub state match convention (protection on `main`), not by a second signal
  that can diverge from the first (`rules/taste.md`: two signals never encode the same
  meaning).
- 2026-09-29, author: the workflow runs `bump mint`, built from the pinned `bump` ref,
  instead of re-deriving the version in shell. One ladder, tested in Rust; a shell copy
  would drift on `skip-members` and workspace inheritance. Panel round 1, both seats:
  agreed.
- 2026-09-29, panel round 1, both seats, author converged: personal repos (scottidler,
  otto-rs) get their rulesets, environments and secrets from a `gh api` sweep script,
  `bin/tag-guard-sweep.sh` in this repo, over an explicit repo list. The author's draft
  recommendation (wire `github-setup-rs` user mode, whose `--user` flag is declared and
  dead at `src/cli.rs:60-62`) is recorded as the follow-on if the list drifts; per
  `rules/taste.md`, make drift a problem before building the tool for it. The script
  lives here because the ruleset is the server half of the invariant `bump` enforces.
- 2026-09-29, panel round 1 (staff seat), author agreed: one ruleset became two, because
  a bypass actor bypasses every rule in its ruleset. `release-tags-create` carries the
  App; `release-tags-immutable` carries nobody.
- 2026-09-29, panel round 1 (staff seat), author agreed: the minting key moves from org
  secrets to a per-repo GitHub Environment with a `main`-only branch policy, and the
  doc's guarantee is narrowed to "no `v*` tag without a merge to `main`".
- 2026-09-29, panel round 1 (both seats), author agreed: `bump finish` keeps the
  moving-tip rule (`tag.rs:73-101`) instead of pinning to the sha it started on.
- 2026-09-29, panel round 1, author pushed back: `concurrency` displacing a pending run
  needs no recovery path. Round 2 (staff seat) refuted it with the out-of-order CI
  completion sequence (A -> B -> C, CI(C) first, `mint(B)` displaces `mint(C)`). Author
  withdrew the pushback; `mint` now follows the tip on the same version and re-dispatches
  for the tip on a different one (API Design).
- 2026-09-29, panel round 1 (staff seat), author agreed with the finding and chose the
  tool-side fix: repos whose only workflow is `tag.yml` would trip the zero-checks
  refusal (`ci.rs:114`); `mint` subtracts the caller file from the workflows tree.
  Round 2 narrowed it: tag-only and PR-only workflow trees still need the committed
  `ci: none` fact, which the sweep writes from the parsed triggers. The declaration
  already exists for exactly this; the tool does not grow a YAML trigger parser.

## Alternatives Considered

### 1. Declared `gate: gated|ungated` in `bump.yml`
- **Description:** bump reads convention from a committed fact when GitHub is silent.
- **Pros:** no org settings change; fixes today's incident in one field.
- **Cons:** a second signal beside the API; the two will diverge and the agent will have
  to choose which to trust, which is the failure that happened today.
- **Why not chosen:** protect `main` in GitHub and the probe is truthful.

### 2. Workflow derives the version in shell, no `bump` in CI
- **Description:** `tomllib` one-liner reads the root manifest, `git tag`, `git push`.
- **Pros:** no build step; the workflow is 20 lines.
- **Cons:** a second implementation of the ladder (workspace inheritance, `skip-members`,
  tip-equality, CI wait, idempotent re-run); not covered by bump's tests.
- **Why not chosen:** kill the class with shared code. Build time is measured in Phase 0(d);
  if it hurts, prebuilt `bump` binaries (the scottidler `binary-release.yml` shape) are the
  follow-on, recorded here so it is not re-litigated.

### 3. Per-repo rulesets through `github-setup-rs` for tatari-tv
- **Description:** add a `rulesets:` facet to the Rust tool and apply per repo.
- **Pros:** one schema for both orgs.
- **Cons:** the Rust tool is not what CI applies (the Python `github-setup` is); its
  config is stale; `managed: false` repos are skipped entirely (`actions.rs:63-67`), which
  is exactly the eight CLI/service repos this doc targets.
- **Why not chosen:** one org ruleset keyed on a custom property is the mechanism already
  in force (ruleset 126206 on `managed`) and needs no new tool code for tatari-tv.

### 4. Reuse `tatari-skills-versioning` as the minter
- **Description:** widen the existing App's installation.
- **Pros:** zero registrations.
- **Cons:** name says skills, does marquee; carries `pull_requests: write` the minter never
  needs.
- **Why not chosen:** naming truth and the permission delta, both against `rules/taste.md`.

### 5. release-plz / release-please
- **Description:** bot-authored release PR, tag cut by the tool on merge.
- **Pros:** industry-standard; kills "forgot the bump" structurally.
- **Cons:** replaces `bump`; per-crate tags by default (`rules/git.md:14`); the version
  level comes from conventional commits the agents write; release-plz #1799 shipped the
  wrong-sha bug this doc guards against.
- **Why not chosen:** the fleet already has the verb; this doc changes who creates the ref,
  not the release model.

### 6. Hook and prose only, no server-side rule
- **Description:** harden `git-release-guard.sh` further.
- **Pros:** no GitHub changes.
- **Cons:** every layer stays on the agent's side; a raw `git -C` or a different agent
  bypasses it. The record shows this does not hold.
- **Why not chosen:** the goal is prevention, not another rule.

### 7. Minting key as an org-level Actions secret
- **Description:** one org secret restricted to the `tags: ci` repos.
- **Pros:** one place to set, no per-repo environment.
- **Cons:** every workflow in every one of those repos can read it, on any ref, so a PR
  workflow could mint a tag without `bump mint` ever running.
- **Why not chosen:** panel round 1 named the route; the `main`-only environment closes
  the PR-time half of it and makes the rest a reviewed merge.

### 8. Reusable workflow homed in `tatari-tv/github-actions`
- **Description:** house home for reusables (`platform-promote-prod.yml`, `semver.yml`).
- **Pros:** in-org, no outside-repo policy question.
- **Cons:** the workflow is `bump`'s and versions with it; personal repos need it too;
  two copies drift.
- **Why not chosen:** single source in `scottidler/bump`; a wrapper in `github-actions` is
  the fallback if Phase 0(c) fails.

### 9. `github-setup-rs` user mode for personal-repo rulesets
- **Description:** make the dead `--user` flag true and add a `rulesets:` facet.
- **Pros:** one tool, one schema, both orgs.
- **Cons:** the Rust tool is not the live applier for tatari-tv either; a schema facet,
  GraphQL user enumeration and a second config file, for a list of ~51 repos that changes
  rarely.
- **Why not chosen:** both panel seats and the author converged on the `gh api` sweep
  (Resolved Decisions). Recorded here as the follow-on if the list drifts.

## Technical Considerations

### Dependencies
- GitHub: rulesets with `Integration` bypass; org custom properties; App installation
  tokens (`actions/create-github-app-token`). All in use at tatari-tv today.
- `bump`: no new crates expected; `mint` composes existing seams. Verify at Phase 2.
- Cross-repo: `scottidler/bump`, `scottidler/claude`, `tatari-tv/github-setup` (config
  only), `tatari-tv/marquee`, then every repo in the fleet table.

### Performance
- One `cargo install` of `bump` per merge to main per repo. Measured in Phase 0(d); the
  prebuilt-binary follow-on is Alternatives 2.
- `bump finish` waits for CI plus the mint run; today it waits for CI only. The delta is
  the mint job's duration.

### Security
- The App private key is the one secret that can mint. Both personas: an environment
  secret in each repo's `tag-minter` environment (`main`-only branch policy), set by `gh
  secret set --env tag-minter` from the workstation in the sweep, rotated by
  re-registering the App and re-running the sweep, never committed, never echoed (the
  secret-echo guard applies). No org-level or repo-level copy exists.
- The bypass list holds one Integration. No role, team or user. Scott included: a
  workstation push by an org admin is rejected (Phase 0(a) proves it).
- Break-glass when the workflow is broken: fix it by PR. If a release cannot wait, the
  order is fixed: (1) set `release-tags-create` to `disabled` (personal) or remove the
  repo's `tags` property by editing `github-setup.yml` (org; editing the property by hand
  is undone by the three-hourly reconcile), (2) commit `tags: local` in `bump.yml`, (3)
  release with the local verbs, (4) reverse both. Every step is visible in git and the
  org audit log. Adding a human to the bypass list is not a break-glass path.
- The environment placement bounds who can use the key: a job reads it only on a ref
  already on `main`. A workflow merged to `main` that misuses it is a reviewed diff. The
  doc's guarantee is worded to exactly that (Components).
- A compromised CI can mint a tag on the tip of main. It could already push to main
  under `contents: write` Apps today; this doc adds no new reach.

### Testing Strategy
- bump: unit and fixture tests named per phase, bare-origin doubles, break-the-code
  proofs recorded in implementation notes.
- Workflow: this repo dogfoods `mint.yml` first (Phase 4); `actionlint` in `otto ci`.
- Hook: self-test matrix rows; new rows shown failing against the old hook.
- Live: Phase 0 scratch repos, then marquee's first minted release probed end to end
  with `sdv probe`.

### Rollout Plan
- Ruleset in `evaluate` until marquee's first minted tag lands, then `active`. `evaluate`
  is a plan feature (Enterprise for org rulesets, absent on personal repos); where it is
  missing, Phase 0's scratch proof stands in and the ruleset goes straight to `active`.
- marquee first (deploys prod on the tag; largest blast radius, so it goes first with
  eyes on it), then tatari-tv in one sweep, then scottidler.
- Rollback per repo: the break-glass order above, ruleset first, fact second, never the
  other way round (a `tags: local` repo under a live `creation` rule has no working
  release path at all). The old local path stays in `bump` for `tags: local` repos until
  the sweep is complete; it is not deleted by this doc.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Org Actions policy blocks outside reusable workflows | Med | Low | Phase 0(c); wrapper in `tatari-tv/github-actions` (Alternatives 7) |
| App-token tag push does not fire `release.yml` | Low | High | Phase 0(b) proves it before any code; in-org precedent `argocd-diff-action/release.yml` |
| `cargo install` per merge is slow | Med | Low | Phase 0(d) measures; prebuilt binaries follow-on |
| `main` protection changes how agents ship on 16 tatari-tv repos | High | Med | It is the org policy; Open Question 2 sets the set; gated flow already works on clyde |
| Two merges race the mint | Low | Low | tip-equality check; `superseded` exit 0; newest tip wins by ruling |
| Personal-repo secrets sweep drifts as repos are created | Med | Low | `create-repo` skill gains the two `gh secret set` lines and the ruleset call (follow-on, noted) |
| `finish` timeout while the mint job queues | Med | Low | timeout names the Actions URL; re-run `finish` is idempotent |
| A workflow merged to `main` misuses the environment key | Low | High | `main`-only environment policy makes it a reviewed diff; the guarantee is worded to that; `release-tags-immutable` keeps such a tag from being moved to hide it |
| `github-setup` reconcile re-applies a property someone removed by hand | High | Low | the config file is the only place the property is edited; break-glass order names it |

## Open Questions

Three remain, all Scott's. Neither reviewer seat can close them; both flagged them as
owner scope.

- [ ] **1. Scott confirms the reversal** of the 2026-07-06 and 2026-09-26 rulings against
  CI-minted tags, and its consequence on marquee: merging a PR that carries a version
  bump IS the prod release. The tag is minted when CI on the merge is green, and
  `release.yml` promotes prod from it; no workstation step sits between merge and prod.
  `bump finish` becomes the install-and-probe step, not the release gate. This doc was
  ordered after the research fan-out; the explicit word is still his.
- [ ] **2. Which tatari-tv repos get `main` protection.** Rec: all 16 in scope (org policy
  requires PR review for AI-generated code; clyde already runs gated). Alternative: marquee
  only, and the others stay ungated with `tags: ci`.
- [ ] **3. App name.** `tag-minter` is the working name in this doc.

## References

- `~/HALL-OF-SHAME.md`, entries 2026-09-18 through 2026-09-29 (marquee), and the tally.
- `docs/design/2026-07-06-release-verbs-and-language-adapters.md:398-406`
- `docs/design/2026-09-26-one-release-command.md:193,318,335-339`
- `~/repos/scottidler/claude/HOME/.claude/hooks/git-release-guard.sh`
- `~/repos/tatari-tv/marquee/.github/workflows/release.yml`, `docs/deploy.md:120-143`
- `~/repos/tatari-tv/github-setup/github-setup.yml:129-137`, `github_setup.py:471`
- `~/repos/tatari-tv/tatari-skills/.github/workflows/auto-version.yaml:22-40`
- `~/repos/tatari-tv/github-actions/.github/workflows/platform-promote-prod.yml:78-90,156-160`
- GitHub docs: rulesets REST (`target`, `bypass_actors.actor_type`), "Triggering a workflow
  from a workflow" (GITHUB_TOKEN does not create runs), available rules ("Restrict
  creations: only users with bypass permissions can create ... tags").

## Addendum A: Fleet table (2026-09-29)

repo | shape | tag-triggered workflow | deploys on tag | bump.yml | latest v*

tatari-tv (16): marquee | workspace | release.yml | prod | no | v1.22.1; clyde | workspace |
release.yml | no | yes | v0.25.8; persona-cli | single | release.yaml | no | no | v1.9.2;
slack-cli | single | release.yml | no | no | v0.14.4; pagerduty-cli | single | release.yml |
no | no | v0.7.7; drata-cli | single | release.yml | no | no | v0.1.4; sdv | single |
release.yml | no | no | v0.5.4; github-setup-rs | single | release-and-publish.yml | no | no
| v0.1.4; ralph-wiggum-loop | single | release.yml | no | no | v0.1.4; whitespace | single |
release.yml | no | no | v0.1.7; rust-cli | single | ci.yml (`tags: v*`) | no | no | v0.5.2;
mcp-io-rs | single | none | no | no | v0.4.0; okta-auth-rs | single | none | no | no |
v0.7.0; renew | single | none | no | no | v0.4.0; claude-pricing | single | none | no | no |
v2.0.0; vault-for-brands-api | workspace | release events, not tag push | via release event
| no | v0.1.7.

otto-rs (1): otto | single | release-and-publish.yml | image to ghcr only | no | v2.6.0.

scottidler (50): with a tag-triggered workflow (16): dashify, gx, mermaid-rs,
obsidian-bookmark, pyr, readtime, requote, rkvr, taskstore, yl (`binary-release.yml`);
eratosthenes, manifest, paii, qai, signal-rs (`release.yml`); scan (`release.yaml`).
Tagged, no workflow (34): aws-tools, bump, cidr, clapr, cli-workspace, cxn, deppy,
expand-tilde, forge, imap-filter-rs, imap-filter-rs-v2, kat, keyby, keyby-rs, Kondo,
layer-config, lint-unused, multi-account-github-mcp, namify, nerf, pyze, repo, scaffold,
scrabbler, slam, spanish-verbs, ssl, stow, tmp, viewport, whitespace, workweek, xray, ytx.

Excluded (no `v*` tag and no tag workflow): tatari-tv/{catalog-api, rust-axum-svc,
rust-catalog}, scottidler/{obsidian-link, otto-old, rust-version, sitr}.

## Addendum B: Research fan-out summary (2026-09-29)

Five slices, all URLs fetched by the researcher that cites them. Reddit was unreachable
from the sandbox; that slice is Hacker News only.

- Ruleset restricts tag creation to a bypass list; `github-actions[bot]` cannot be a
  bypass actor, an App or deploy key can. docs.github.com available-rules-for-rulesets;
  REST `actor_type: Integration | DeployKey | ...`; community discussion 25305 (GitHub
  staff on why the Actions token is excluded). Confirmed ruleset exports:
  `actana/control` `docs/rulesets/tag-release-cut.json`, `ggml-org/whisper.cpp`
  `ci/ruleset-official.json`.
- `GITHUB_TOKEN` pushes do not create workflow runs; App tokens do. docs.github.com
  "Triggering a workflow from a workflow"; release-plz token docs.
- Release-PR tools tag from CI after merge: release-plz, release-please (rust strategy),
  knope, changesets. release-plz #1799: tagged the pre-squash head, fixed by resolving the
  merged sha. cargo-release: `verify_if_behind`, `tag_exists`, `--atomic` branch+tag push.
- Hooks over prose: Edward Blake, "Why Your Claude Code Rules Get Ignored" ("The model
  never sees a choice ... This is not a question of model quality. It's a question of
  architecture."); HN 46728766 (Cursor force-pushed despite rules; "the tool call itself
  needs to be gated"); karakun postmortem (agent deleted main; fix was protection plus a
  scoped token, "the instruction file is the cheap layer, never the mechanism").
- Deny-rule bypass: Claude Code permissions doc says `Bash(git push *)` is bypassed by
  `git -C . push`; dev.to post on `git -C /path commit` evading a substring guard.
- Bump level: no source defends an operator flag; epage (cargo-release) on HN 31480306:
  the next level "is pure speculation" until release time; RustConf 2024 (Gruevski)
  derives it with cargo-semver-checks.
- Ancestry/equality check before deploy: proposed in two GitHub issues, shipped by no
  tool; marquee already uses equality (`platform-promote-prod.yml:156`).

## Addendum C: Review pass log

- Pass 1, draft: shape, four layers, ten phases, fleet table, research summary.
- Pass 2, correctness: `github.workflow_sha` replaced (the `github` context in a
  reusable workflow is the caller's); the pass picked `github.job_workflow_sha`, which
  the panel round below corrected to `job.workflow_sha`. otto-rs is a third org: it
  rides the home App, not a third registration. Versioning-App reuse delta corrected to
  16 repos.
- Pass 3, clarity: phase count and operator phases named up front; second caller variant
  shows its secrets; `superseded` and red-CI behaviors written out under Architecture.
- Pass 4, edge cases: `evaluate` enforcement is a plan feature, fallback stated; sweep
  reads each repo's CI `name:`/`on:` to pick the caller variant; a reverted version at
  the tip fails the mint run loudly (`version reused`), by design. Right problem? The
  invariant becomes true by construction; what the agent still chooses is the level and
  the merge, both outside this doc's scope and named as such.
- Pass 5, excellence: voice lint (no dashes as asides, no "real", one hedge max); every
  acceptance criterion carries its observed value; every alternative names why not.
- Panel round 1 (architect: Gemini, staff engineer: Codex; synthesis
  `/tmp/review-panel/hW1FlB5S/synthesis.md`): verdict not ready, must-fix 11, cheap-win
  10, defer 1. Folded: `finish` keeps the moving-tip rule; two rulesets instead of one;
  key in a `main`-only environment and the guarantee narrowed; `job.workflow_sha`, not
  `github.job_workflow_sha` (both seats named it wrong; fetched from the contexts
  reference: "The commit SHA of the workflow file that defines the current job");
  `github-setup` needs a `tags` field, PR added to Phase 6; App gains `checks`,
  `statuses`, `actions` read; `GH_TOKEN` and tagger identity in the job; both re-verifies
  restored in `mint`; `no new version` exit-0 path; caller-file subtraction for no-CI
  repos; run identity kept in the check-runs parser; `BUMP_GATES_PROBE` debug-only;
  `workflow_dispatch` retry that doubles as the fleet probe; Phase 9 criterion given an
  end point; `rules/git.md` in Phase 7; hook scope (any tag) dispositioned; rollback
  order; acceptance criteria measured as ref sets, run provenance, both endpoints, named
  budgets; personal repos moved to the sweep (both seats). Pushed back: concurrency
  displacement needs no recovery (withdrawn in round 2). Owner scope, left to Scott in
  Open Questions: the reversal and merge-is-release, the `main` protection set, the App
  name.
- Panel round 2 (deltas; synthesis appended to the same file): not ready, must-fix 6,
  cheap-win 6. Folded: environment secrets cannot be passed from a caller, so the
  callee's job declares the environment and reads them with `secrets: inherit`, and
  Phase 0(e) proves that exact cross-repo path; concurrency pushback withdrawn, `mint`
  follows the tip on the same version and re-dispatches on a different one, with an
  out-of-order test; `finish` accepts `vN` at an ancestor of the followed tip (immutable
  tags cannot move to an unbumped tip); tag-only and PR-only trees need `ci: none`,
  written by the sweep from parsed triggers; the github-setup `tags` apply clears on
  absence (the `managed` copy would skip) with a set/clear/readback test; `no new
  version` only when no higher tag sits between `vN` and the tip, so a reverted manifest
  fails loudly as Pass 4 recorded; Security section brought in line with the
  environment placement; `head_repository` check on the `workflow_run` caller against a
  fork branch named `main`; tagger email uses the bot user id; Phase 9 check split into
  credential smoke, trigger class, first release; environment assertion reads the
  `deployment-branch-policies` endpoint; generic repos refuse in `mint` and before the
  `finish` wait.
