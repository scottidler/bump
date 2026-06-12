# Design Document: Gated-Repo Tagging

**Author:** Scott Idler (drafted with Claude)
**Date:** 2026-06-12
**Status:** Draft
**Review Passes Completed:** 5/5

## Summary

`bump` currently assumes the commit it tags will reach `origin/<default-branch>` by a
direct push. On repos where the default branch is gated (classic branch protection
and/or GitHub rulesets, e.g. an org-level required-workflow ruleset), that assumption
is structurally false: the bump commit must ride a PR, squash-merge rewrites the SHA,
and the tag `bump` created points at a commit that will never be on main - an orphaned
tag. This design adds gate detection to `bump`, makes it refuse to tag where the tag
would orphan, and adds `--no-tag` and `--tag-only` modes so the same tool covers both
the ungated and gated release flows.

## Problem Statement

### Background

The release flow `bump` was built for:

```
commit on main -> bump (version + commit + tag, atomically) -> git push && git push origin vX.Y.Z
```

This works on any repo where the operator can push the default branch directly. It has
orphaned tags three times on repos where they cannot:

- `cr` v0.1.8 - tagged a stale local main
- `claude-pricing` v0.2.0 - direct push rejected by an org required-workflow ruleset
  that the classic branch-protection API does not report
- `okta-auth-rs` v0.2.0 - same ruleset rejection, compounded by `push.followTags`
  pushing the tag even though the branch push was rejected

The common root: gating is decided by **two independent GitHub layers** - classic
branch protection and rulesets (repo- and org-level). Org rulesets are not visible via
`GET /repos/{o}/{r}/branches/{b}/protection` and are not bypassed by repo-admin
permissions (`enforce_admins: false` only bypasses the classic layer). In the tatari-tv
org, the ruleset layer follows the `managed` custom repository property set by
github-setup; `managed: true` (the default) means the default branch only accepts
merges via PR with a passing required workflow - for everyone, admins included.

On such repos, the SHA `bump` tags can never be the SHA that lands on main
(squash-merge rewrites it), so running `bump` as designed is guaranteed to produce an
orphaned tag. Guidance in prose (skills, rules files, memories) has failed to prevent
this three times; the tool itself must refuse.

### Problem

`bump` tags local HEAD unconditionally, with no knowledge of whether that commit can
ever reach the remote default branch. On gated repos this is always wrong, and the
failure (an orphaned tag on the remote) is expensive to recover because tags are
immutable by policy (never deleted or moved except manually by the owner).

### Goals

- `bump` detects, per repo, whether the default branch is gated (both layers).
- On a gated repo, the default invocation refuses to tag and prints the correct
  gated-flow recipe instead of producing a guaranteed orphan.
- `--no-tag`: bump version + commit (or amend) without tagging, so the version bump
  can ride a feature branch / PR.
- `--tag-only`: after the bump commit has merged, create the annotated tag on the
  merged commit - with hard verification that HEAD is identical to
  `origin/<default-branch>` first.
- `--gates`: report both gate layers and the recommended flow, then exit (replaces the
  interim `tagit gates` shell script).
- Single tool, single mental model: "use bump; it knows the waters." Skills and rules
  collapse to one sentence each.

### Non-Goals

- **Pushing.** `bump` never pushes commits or tags; it prints the exact push commands.
  Push-ordering safety (branch first, verify, then tag by explicit name) stays in the
  operator's hands, backstopped by `push.followTags=false` and harness deny rules.
- **PR creation.** Opening/merging the PR is the operator's (or a skill's) job.
- **Tag deletion or moving.** Never. Recovery of an existing orphan is out of scope.
- **CI-side tagging** (tag-on-merge workflows). Complementary org-level work, not a
  `bump` feature.
- **Per-crate / multi-scheme tags.** Single flat `v*` scheme, unchanged.

## Proposed Solution

### Overview

A new `github` module probes the repo's remote once per invocation and classifies the
default branch as `Ungated`, `Gated`, or `Unknown`. The main flow consults the
classification before any tag is created:

| Invocation | Ungated | Gated |
|---|---|---|
| `bump` (default) | current behavior: version + commit + tag | **error**: prints gated recipe, exits non-zero, no changes made |
| `bump --no-tag` | version + commit, no tag | version + commit, no tag |
| `bump --tag-only` | tag HEAD if it matches manifest version and equals `origin/<default>` | same (this is its primary use case) |
| `bump --gates` | report and exit 0 | report and exit 0 |

The two flows, end to end:

```
# ungated (managed: false / no protection):
bump [-m|-M]                      # version + commit + tag
git push origin main              # verify it lands
git push origin vX.Y.Z            # tag last, by explicit name

# gated (managed: true / any protection or ruleset):
bump --no-tag [-m|-M]             # version bump rides the feature branch
git push origin <branch> ; open PR ; merge
git checkout main && git pull --ff-only origin main
bump --tag-only                   # annotated tag on the merged SHA
git push origin vX.Y.Z
```

### Architecture

New module `src/github.rs`, following the established pattern in `gx`
(`gx/src/github.rs`): all GitHub access shells out to `gh`, commands are built
through one helper that injects per-org auth (`GH_TOKEN` from the org's token file
when present, ambient `gh auth` fallback with a `debug!` note), and network calls go
through a retry helper with exponential backoff on retryable errors. This matters for
bump specifically because it runs across both identities (scottidler and tatari-tv
repos); per-org tokens keep the probe authed as the right account without any
environment juggling.

```rust
pub enum Gate {
    Ungated,            // both layers clear
    Gated(Vec<String>), // blocking rule types, e.g. ["pull_request", "workflows"]
    Unknown(String),    // probe failed: reason (no remote, non-GitHub, gh error, offline)
}

pub fn detect(path: &Path) -> Gate;
```

Detection steps (all shelling out, consistent with `git.rs`):

1. **Slug**: parse `git remote get-url origin` (SSH and HTTPS forms). No `origin` or a
   non-GitHub host => `Unknown` ("not a GitHub remote").
2. **Default branch**: `git symbolic-ref refs/remotes/origin/HEAD` (fallback:
   `gh api repos/{slug} --jq .default_branch`).
3. **Classic layer**: `gh api repos/{slug}/branches/{branch}/protection`
   - HTTP 200 => gated (contributes `"classic_protection"`)
   - HTTP 404 => clear
4. **Ruleset layer**: `gh api repos/{slug}/rules/branches/{branch}`
   - Filter out `deletion` and `non_fast_forward` (they do not block a normal push).
   - Any remaining rule types (`pull_request`, `workflows`,
     `required_status_checks`, ...) => gated, listing those types.
5. Any other failure after retries (gh missing, not authenticated, network down) =>
   `Unknown` with the reason.

`gh` is the auth boundary: it already handles tokens, hosts, and enterprise setups,
and is a hard prerequisite of every flow that ends in a push anyway. It joins `git`
in the REQUIRED TOOLS validation in `cli.rs` help output (gx validates the same pair
in its doctor).

**`Unknown` policy - warn and proceed as ungated.** Tag creation is local and
recoverable until pushed; the catastrophic step is the push, which `bump` never
performs. Failing closed would break the most common invocation (offline bump on a
personal repo). The warning states exactly what was not verified and tells the
operator to run `bump --gates` once online. `--no-verify` silences the probe entirely
(skips it, treats as ungated, no warning) for air-gapped or scripted use.

### Data Model

No persistent state. `Gate` is computed per directory argument per invocation (multi-
directory invocations probe each repo independently). No caching in v1: the probe is
two `gh api` calls (~half a second) on a path that ends in a manual push - not worth
staleness bugs. Revisit only if it ever annoys in practice.

### API Design (CLI)

New flags on the existing flag-based CLI (no subcommands; matches current shape):

```rust
/// Bump version and commit, but do NOT create a tag (for PR-gated repos)
#[arg(long, conflicts_with_all = ["tag_only"])]
pub no_tag: bool,

/// Create the tag for the current manifest version on HEAD (post-merge step
/// for PR-gated repos). No version change, no commit.
#[arg(long, conflicts_with_all = ["no_tag", "major", "minor", "force", "message", "automatic"])]
pub tag_only: bool,

/// Report gate status (classic protection + rulesets) and the recommended flow
#[arg(long)]
pub gates: bool,

/// Skip the remote gate probe (treat repo as ungated)
#[arg(long)]
pub no_verify: bool,
```

`--gates` composes with nothing; it reports and exits.

**Behavior changes in `process_directory`:**

- Default path: call `github::detect` before `determine_version_action`. On
  `Gated(rules)` print:

  ```
  bump: ERROR: 'main' on tatari-tv/okta-auth-rs is gated (pull_request, workflows).
  Tagging here would orphan the tag (squash-merge rewrites the SHA).

  Gated flow:
    bump --no-tag [-m|-M]      # version bump rides your branch/PR
    <push branch, open PR, merge>
    git checkout main && git pull --ff-only origin main
    bump --tag-only            # tag the merged commit
    git push origin vX.Y.Z
  ```

  and exit non-zero **before any file or git mutation**.

- `--no-tag` path: identical to the default path minus tag creation, and minus the
  gated refusal (this flag is valid on both flows). The existing version-action rules
  (initial tag, tag-matches-bump, defer-to-tag, managed mismatch errors) still apply
  for computing the next version; only the `create_tag` step is skipped. Output names
  the version and reminds that the tag comes later via `--tag-only`.

- `--tag-only` path, in order, all of which must hold or it exits non-zero with no
  mutation:
  1. working tree clean (`has_uncommitted_changes` == false)
  2. current branch == remote default branch
  3. `git fetch origin <default>` then `HEAD` == `origin/<default>` exactly
     (not merely an ancestor - "behind" and "ahead" are both errors with distinct
     messages)
  4. manifest version `X.Y.Z` parses; tag name is `vX.Y.Z`
  5. tag existence check, locally and on the remote
     (`git ls-remote origin refs/tags/vX.Y.Z`): missing => create; existing and
     pointing at HEAD => succeed idempotently ("already tagged"); existing and
     pointing anywhere else => error - resolving that is operator-level tag
     surgery, never bump's
  6. create annotated tag via existing `git::create_tag`
  7. print `git push origin vX.Y.Z` as the next step

  `--tag-only` runs on both flows (on an ungated repo it is simply a tag-the-merged-
  commit convenience) so it performs the gate probe only to label its report, never to
  refuse.

- `--dry-run` composes with all of the above (prints what would happen, including the
  gate verdict).

### Implementation Plan

#### Phase 1: github module
**Model:** sonnet
- `src/github.rs` modeled on gx's: `gh_command` (per-org `GH_TOKEN` injection with
  ambient fallback), `retry_gh` (exponential backoff on retryable errors), then the
  feature surface: `Gate` enum, `detect()`, slug parsing (SSH + HTTPS),
  default-branch resolution, the two `gh api` probes, rule-type filtering
- Function-level debug logging per logging.md (probe inputs, HTTP outcomes, verdict)
- Unit tests for slug parsing and rule classification; probe seam via a
  `BUMP_GATES_PROBE` env override (`ungated|gated:<types>|unknown:<reason>`) so tests
  and the e2e suite never hit the network
- Add `gh` to REQUIRED TOOLS validation in `cli.rs`

#### Phase 2: gated refusal wired into the default path
**Model:** opus
- Call `detect()` in `process_directory` before any mutation; implement the refusal
  error and the `Unknown` warn-and-proceed policy; `--no-verify`
- Careful interaction with multi-directory invocations (per-repo verdicts, aggregate
  exit code) and with `--dry-run`
- Tests: gated refusal happens before any file change; unknown proceeds with warning

#### Phase 3: --no-tag
**Model:** sonnet
- Thread a `create_tag: bool` through the action execution; skip tag creation and
  adjust output/log lines
- Tests mirroring the existing rule_* matrix with `--no-tag` asserting no tag exists
  afterward

#### Phase 4: --tag-only
**Model:** opus
- The seven-step verification ladder above; exact-equality check against
  `origin/<default>`; idempotent already-tagged case; remote-tag conflict error
- New `git.rs` primitives: `fetch_branch`, `remote_default_branch`, `remote_tag_sha`,
  `head_equals_remote`
- Tests: each ladder step failing in isolation produces its distinct error and no tag

#### Phase 5: --gates report, help text, docs, ecosystem cleanup
**Model:** sonnet
- `--gates` human-readable report (mirrors the interim `tagit gates` output)
- README + `after_help` examples for both flows
- Retire the `tagit` shell script; update the bump/shipit skills and git.md rule to
  the one-sentence forms pointing at `bump --gates` / `--no-tag` / `--tag-only`

## Alternatives Considered

### Alternative 1: Keep the flow in a wrapper script (tagit)
- **Description:** the interim shell script that probes gates and sequences
  bump/push/tag.
- **Pros:** already written; zero Rust work; can also own push ordering.
- **Cons:** a second tool agents must know to reach for - the failure mode this
  design exists to kill is "the agent didn't follow the right doc"; shell, so no test
  matrix like bump's; duplicates bump's version logic for the PR path.
- **Why not chosen:** consolidation is the point. tagit is the prototype; bump is the
  home. (tagit's gate semantics and verification ladder transfer directly.)

### Alternative 2: Native GitHub API client (octocrab / ureq)
- **Description:** call the REST endpoints directly instead of shelling to `gh`.
- **Pros:** no runtime dependency on `gh`; typed responses.
- **Cons:** token discovery/auth (multi-account, enterprise hosts) is exactly the
  mess `gh` already solves; adds a network stack to a tool that is otherwise
  pure-git; more code to test.
- **Why not chosen:** `gh` is already a prerequisite of every gated flow (PRs), and
  bump already shells out for all git operations - same pattern, same auth story.
  gx made the same call (`gx/src/github.rs`) and its per-org token + retry helpers
  are the template; staying consistent across the tool fleet beats a second auth
  implementation.

### Alternative 3: Read the org's github-setup.yml `managed` bit
- **Description:** consult the tatari github-setup config (the source that drives the
  org ruleset targeting) instead of the live API.
- **Pros:** one bit, no API semantics.
- **Cons:** tatari-specific; requires a local checkout of a private repo; the live
  org state can drift from config (observed: three repos whose live protections
  diverged from config); useless for personal/other-org repos.
- **Why not chosen:** the live `rules/branches/{branch}` endpoint is ground truth for
  any repo on GitHub, org-agnostic.

### Alternative 4: CI tags on merge (tag-on-merge workflow)
- **Description:** a workflow in gated repos that creates `vX.Y.Z` from the manifest
  when a bump commit lands on the default branch; humans and agents never tag.
- **Pros:** eliminates the failure class by construction; the only approach that is
  correct even when every client-side layer fails.
- **Cons:** per-repo rollout and an org convention conversation; does nothing for
  ungated personal repos, so bump still needs its own story.
- **Why not chosen:** not an alternative but a complement; pursue separately for
  tatari gated repos. bump's `--tag-only` remains correct alongside it (idempotent
  already-tagged case).

## Technical Considerations

### Dependencies
- Runtime: `git` (existing), `gh` (new, validated in help/REQUIRED TOOLS; absence
  degrades to `Unknown` + warning, never a hard failure of ungated flows)
- No new crates required; probes go through `std::process::Command` like `git.rs`

### Performance
- Two `gh api` calls per repo per invocation on the default path (~300-600ms total) -
  acceptable for an interactive release tool
- `--no-tag` skips the probe entirely (no tag is created, so there is nothing to
  refuse), keeping the PR-flow inner loop fast; `--no-verify` skips it on the
  default path

### Security
- No tokens handled directly; all auth delegated to `gh`
- Probe output (rule types) logged at debug; no secrets in logs
- Refusal-by-default means a compromised/confused caller cannot produce a remote
  orphan via bump; the remaining exposure (raw `git push --tags`) is mitigated
  outside bump (followTags=false, harness deny rules)

### Testing Strategy
- Unit: slug parsing, rule classification, version-ladder errors (existing rule_*
  test pattern extended)
- Probe seam: `BUMP_GATES_PROBE` env override; integration tests also cover a fake
  `gh` shim on PATH returning canned 200/404/error responses
- e2e: temp repos with a bare "origin" remote exercise `--tag-only`'s
  exact-equality and idempotency checks without GitHub

### Rollout Plan
- Ship as a minor bump of `bump` (new flags; default-path behavior changes only on
  gated repos, where the old behavior was always wrong)
- Update skills (bump, shipit) and git.md in the same change set; delete tagit
- Validate on the next real release of a gated repo (okta-auth-rs successor release)

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Probe misclassifies a gated repo as ungated (API change, new rule type) | Low | High | Unknown/odd responses log verbatim; rule filter is allowlist-of-harmless (`deletion`, `non_fast_forward`), so new rule types fail toward Gated, not Ungated |
| `gh` missing/unauthenticated on a machine that needs gating | Med | Med | Loud `Unknown` warning naming the missing verification; REQUIRED TOOLS surface in `--help` |
| Probe latency annoys ungated daily use | Med | Low | `--no-verify`; revisit caching only if real |
| `--tag-only` run on wrong commit (operator skipped the pull) | Med | High | Exact HEAD == origin/default equality check is the whole point of the ladder; ahead/behind produce distinct, instructive errors |
| Old muscle memory runs plain `bump` on a gated repo in a script | Med | Low | Refusal is an error exit before mutation - scripts fail loudly instead of orphaning |

## Open Questions

- [ ] Should `--tag-only` also verify that the merged HEAD's *content* matches what
  the operator expects (e.g. the bump commit's tree), or is version-string equality
  sufficient? (Current answer: version equality is sufficient; the tag is defined by
  the manifest on main.)
- [ ] Is `--gates` worth promoting to the default output of `bump --dry-run`?

## References

- gh-invocation pattern to inherit: `~/repos/scottidler/gx/src/github.rs`
  (`gh_command` per-org token injection, `retry_gh` backoff, doctor tool validation)
- okta-auth-rs incident session: `e8881722-ffbd-43a5-ada5-b99dcc80d069` (2026-06-11)
- Interim prototype: claude repo `HOME/.claude/bin/tagit`
- Org gating source of truth: `tatari-tv/github-setup` `github-setup.yml`
  (`managed` custom property drives the org required-workflow ruleset)
- GitHub rules API: `GET /repos/{owner}/{repo}/rules/branches/{branch}`
