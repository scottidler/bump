# Design Document: one release command

**Author:** Scott Idler (drafted by Claude)
**Date:** 2026-09-26
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Every release, gated or ungated, becomes one command the agent runs bare: `bump release`, then `bump finish` after a gated merge. The verb opens the PR itself, waits for green CI before any tag exists, takes Scott's standalone order as a flag instead of an env prefix, and finishes from any worktree. The bash driver `~/.claude/bin/release`, the `pr-open` -> `gh pr create` -> `release --pr` hand-back, and the `BUMP_ORDERED_BY_SCOTT=1` door are retired; the hook, skills, agent and rules shrink to point at the two verbs.

## Problem Statement

### Background

- Two release drivers exist. `bump release` / `bump finish` (Rust, `src/release.rs`, shipped 2026-07-06, installed at v0.3.3) and `~/.claude/bin/release` (bash, 460 lines). Every skill (`/bump`, `/shipit`, `/how-to-execute-a-plan`), the `release-driver` agent, `rules/git.md` and the `git-release-guard.sh` hook name the bash one. The Rust one has no caller. The setup-audit baton (`scottidler/claude/docs/design/2026-09-13-setup-audit-program.md:82,91`) recorded this and handed it to "the bump repo's next doc".
- The bash gated flow is five agent-run steps: `release` -> `pr-open ...` -> the printed `gh pr create ...` verbatim -> `release --pr <url>` -> wait -> `release --finish`. It stops short of the PR on purpose so PreToolUse hooks can inspect `gh pr create`. Every hall-of-shame entry since 2026-07 is an agent choosing wrong at one of those seams (`~/HALL-OF-SHAME.md`, 2026-09-08, 09-15, 09-24, 09-25, 09-26).
- The door is an env prefix: `BUMP_ORDERED_BY_SCOTT=1 bump --no-tag`. Under auto mode the prefix stops the command matching the `Bash(bump:*)` / `Bash(git push:*)` allow rules, and `bump`, `git push` and `release` are in `sandbox.excludedCommands`, so the classifier reads prefix + unsandboxed command as `[Auto-Mode Bypass]` and denies. That is the whole of the 2026-09-26 marquee v1.20.1 failure.
- Hook denials mentioning bump, release or tags in the last 14 days of session transcripts: 1. Classifier denial strings in the same transcripts (text occurrences, some quoted in later write-ups): `[Production Deploy]` 14+, `[Auto-Mode Bypass]` 26+, fired on `release`, on read-only `git rev-parse`, and on `gh run list`. The failures sit in the seams and the harness, not in the hook.
- `git::is_head_pushed` (`bump/src/git.rs:122`) consults only `@{u}`. A branch cut from `origin/main` with no upstream reports "not pushed", so `bump --no-tag` amends a commit that is already on origin (marquee `417870a`, 2026-09-26).
- The CI-wait rule (2026-08-31, otto v2.0.4/v2.0.5 double-tap) landed in the bash driver only. `bump release` tags the ungated commit immediately after the push, and `bump finish` tags the merged commit without waiting.
- Scott, 2026-09-26: "IT SHOULD NOT BE HARD ... 1. tagging repos that protect main, require a PR 2. tagging repos that DO NOT protect main, DO NOT require a PR ... only wrinkle, is you have to set the fucking version string in Cargo.toml BEFORE creating the PR, so the bump after matches ... FIX IT."

### Problem

Releasing needs an agent to sequence several commands across two tools and to remember a rule at each seam. Agents fail at seams. The one place that cannot be forgotten is inside the binary that already owns the version.

### Goals

- One bare command per half: `bump release` (either gate), `bump finish` (gated, after merge). No hand-back, no second tool. (Scott, 2026-09-26)
- The version is set before the PR exists, by the same command that opens the PR. (Scott, 2026-09-26: "set the version string in Cargo.toml BEFORE creating the PR")
- No tag exists until the commit it points to is on `origin/<default>` and its CI is green, in both flows. (Scott's double-tap rule, HALL-OF-SHAME 2026-08-31)
- Scott's standalone order is a flag with his words: `bump release --standalone "<words>"`. Same semantics as the door (THE RULING 2026-07-10), usable under auto mode.
- The bump never rides alone without that order: a branch whose diff against the default branch is empty or version lines only refuses. (THE RULING 2026-07-03)
- `bump finish` works from any worktree of the repo. (herdr worktree layout; marquee, 2026-09-25)
- The version commit is never an amend of a commit on origin, and in the release verb never an amend at all.
- One driver. `~/.claude/bin/release` is deleted; the hook, skills, agent and rules name only the two verbs.

### Non-Goals

- The auto-mode classifier's verdicts. This doc makes the release a single bare `bump ...` command that the existing `Bash(bump:*)` allow rule matches; it does not model or tune the classifier.
- Whether `tatari-tv/marquee` should protect `main`. `bump --gates` says ungated, the org policy line says PR review for AI-authored code. Scott's 2026-09-26 framing is binary by protection, so the verbs follow `bump --gates`. Protecting marquee's `main` is a repo-settings decision, parked; revisit if Scott wants PRs on a mechanically ungated repo.
- Non-release PRs. A docs-only PR still goes through `gh pr create` with `Release: none - <why>`; `pr-open` stays for that. Gate D stays for raw `gh pr create`.
- Gated repos with no manifest (generic). Unsupported since 2026-07-06; unchanged.
- Multi-directory batch runs (`bump dir1 dir2`). The verbs run in the current repo only.
- Jobs that run only on the tag push (marquee `release.yml`, otto `release-and-publish.yml` build jobs) are untested by any pre-tag gate; the gate can only see what ran on the commit. otto already moved its shared checks into a push-triggered workflow; that is the per-repo remedy, not this tool's.

## Proposed Solution

### Overview

Port the bash driver's two remaining advantages into `bump release` / `bump finish` (CI wait, PR creation) and add the four behaviors the incidents demand (standalone flag, bump-only refusal, worktree-aware finish, no-amend). Then retire the bash driver and rewrite the prose around the two verbs.

The agent contract, which is all the skills and the agent will say:

```
cd <repo>                                  # its own Bash call
bump release [-m|-M]                       # bare: no &&, no env prefix, no wrapper
# gated: merge the PR, then
bump finish                                # bare, from any worktree
# Scott ordered a version-only release:
bump release --standalone "<his words>"
```

A bare `bump ...` matches the `Bash(bump:*)` allow rule; anything chained or prefixed does not, and that is where the classifier denials came from. The Bash tool's foreground wall clock is 600s (measured in this session: two calls were moved to the background at 600s), and the CI gate waits up to `--ci-timeout` (default 1800s), so the agent runs `bump release` and `bump finish` with `run_in_background` and reads the result when the harness reports the exit. The `release-driver` agent already waits that way for PR checks.

```
UNGATED   bump release [-m|-M]
          version commit -> git push origin main -> confirm HEAD == origin/main
          -> wait for green check-runs on that sha -> git tag -a vX.Y.Z -> git push origin vX.Y.Z -> install

GATED     bump release [-m|-M]                          (on the feature branch, work committed)
          version commit (new commit) -> git push --no-follow-tags -u origin <branch>
          -> gh pr create --head <branch> --base main --title "<type>(<scope>): <branch words>"
                          --body "<subjects>\n\nRelease: rides this PR (vX.Y.Z)"
          -> pause: "merge the PR, then run: bump finish"

          bump finish                                    (from any worktree, after the merge)
          find the worktree holding main -> git pull --ff-only -> tag_ladder (HEAD == origin/main)
          -> wait for green check-runs on the merged sha -> git tag -a vX.Y.Z -> git push origin vX.Y.Z -> install

STANDALONE  bump release --standalone "<Scott's words>"
          gated, on main, clean, == origin, version tagged:
            git checkout -b bump-vX-Y-Z --track origin/main -> the GATED flow above, body quotes Scott
          ungated, on main, clean, == origin, version tagged:
            the UNGATED flow above (the version commit is the release)
```

### Architecture

- `src/release.rs`: the state machine gains states `UngatedStandalone`, `GatedStandalone`, `GatedBumpOnlyBranch`, `GatedBadBranchName`; a `Ci` port next to `Pusher` / `Installer` / `Pr`; `wait_for_green` shared by `execute_release`, `execute_resume` and `finish`; `pr_title` / `pr_body` / `standalone_branch_name` pure functions; `finish_dir` worktree resolution.
- `src/github.rs`: `create_pr(path, branch, base, title, body) -> url` replaces `--fill`; `check_runs(path, sha)` + `check_runs_from_json`; `token_for_org` resolves `GH_TOKEN` as token file -> `GITHUB_PAT_<ORG>` -> `GITHUB_PAT_WORK` for `tatari-tv`, `GITHUB_PAT_HOME` otherwise -> ambient (the dotfiles `gh()` function's mapping, because bump's subprocess `gh` does not see that shell function).
- `src/git.rs`: `is_head_pushed` true when any remote-tracking ref contains HEAD (`git branch -r --contains HEAD`, every remote, not only `origin`), then the `@{u}` check; new `commit_subjects`, `changed_files`, `checkout_new_tracking`, `worktree_for_branch`.
- `src/cli.rs`: `ReleaseArgs` gains `--standalone <WORDS>`, `--no-ci-gate`, `--ci-timeout <SECS>` (default 1800); `FinishArgs` gains the two CI flags; `Cli` gains an internal `#[arg(skip)] never_amend` the release verb sets.
- `src/config.rs`: `bump.yml` gains `ci: none` (the only accepted value), a committed per-repo fact: this repo's workflows never register checks on a push to the default branch. Unknown values are a loud error like every other key. The CI gate reads it with `git show <sha>:bump.yml`, never off the working tree: HEAD's `config::load` reads the filesystem, and the tracked-changes check ignores untracked files, so an untracked `bump.yml` would otherwise switch the gate off.
- `src/main.rs`: the two amend decision sites become `cli.never_amend || git::is_head_pushed(dir)?`.
- `scottidler/claude` (operator phase): `git-release-guard.sh` exempts `bump release|finish` from the legacy bump gates and drops the door; `bin/release` deleted with its `manifest.yml` link and `settings.json` entries; `/bump`, `/shipit`, `release-driver.md`, `rules/git.md`, `/how-to-execute-a-plan` rewritten around the two verbs.

### Data Model

```rust
pub struct ReleaseOpts {
    pub bump_type: Option<BumpType>,  // None: reuse a version already riding, else patch
    pub dry_run: bool,
    pub install: InstallChoice,
    pub standalone: Option<String>,   // Scott's words, verbatim; None = a bump-only release refuses
    pub ci_gate: bool,                // default true
    pub ci_timeout: Duration,         // default 1800s
}
pub struct FinishOpts { pub dry_run: bool, pub install: InstallChoice, pub ci_gate: bool, pub ci_timeout: Duration }

pub trait Pr {
    fn open_pr_exists(&self, dir: &Path, branch: &str) -> Result<bool>;
    fn create_pr(&self, dir: &Path, branch: &str, base: &str, title: &str, body: &str) -> Result<String>;
}
pub trait Ci { fn check_runs(&self, dir: &Path, sha: &str) -> Result<Option<CheckRuns>>; }  // None = no GitHub remote; Err = API/auth failure, fails closed
pub struct CheckRuns {
    pub total: usize,           // check runs returned; refuse if the API's total_count is larger (truncation)
    pub incomplete: usize,
    pub failed: Vec<(String, String)>,
    pub statuses: StatusState,  // legacy commit-status API: Success | Pending | Failure | None
}
// git::create_tag(dir, tag, message, sha): the tag binds to the sha that passed CI, never to implicit HEAD

pub struct ReleaseReport { tag, resumed, paused, install_command, dry_run, pr_url: Option<String> }
```

PR title: `<type>(<scope>): <branch words>` where `type`/`scope` come from the first commit subject on the branch (`feat(core): add thing` -> `feat`, `core`), `chore` when the subject has no conventional prefix, and `branch words` is the branch name with `-` -> space. The invariant `branch-pr-title-guard.sh` enforces is `title_slug(title) == branch`, where `title_slug` strips the `type(scope):` prefix, lowercases, collapses every `[^a-z0-9]+` run to `-` and trims dashes (`branch-pr-title-guard.sh:65-69`). A title built from the branch satisfies it only when the branch is already its own slug, so the verb refuses any branch where `slug(branch) != branch` (`Add-thing`, `add_thing`, `add--thing`, `feat/x`, `v1.2`), printing `git branch -m <slug>`. That is the whole precondition; the `/` and `.` cases are two instances of it.

PR body: one `- <subject>` per commit on the branch; `Standalone release ordered by Scott: "<words>"` when `--standalone`; last line `Release: rides this PR (vX.Y.Z)`, the line Gate D looks for.

Standalone branch: `bump-vX-Y-Z` (dots -> dashes, because the title derives from the branch and `.` never slugifies back). Cut with `--track origin/<default>` so it has an upstream from birth.

Bump-only branch: a port of the hook's `is_bump_only_ref` (`git-release-guard.sh:234-257`) with one intended difference. The hook returns "not bump-only" for zero commits ahead because its Gate A covers that case separately; the verb has no Gate A, so an empty diff IS bump-only here and a fresh-cut branch refuses. Otherwise identical: a branch is bump-only when every changed path is one of the ROOT files `Cargo.toml Cargo.lock package.json package-lock.json pnpm-lock.yaml yarn.lock pyproject.toml uv.lock VERSION` (exact paths, no directory) AND the only change in `Cargo.toml package.json pyproject.toml VERSION` is the PACKAGE version line (Cargo `[package]`/`[workspace.package]` `version`, pyproject `[project]`/`[tool.poetry]` `version`, package.json top-level `"version"`, the whole `VERSION` file). A `version =` under `[dependencies.<name>]` is a dependency line, where the hook's line regex would count it (audit round 1). A lockfile-only refresh (no manifest lines changed) and a dependency bump (a non-version manifest line changed) are NOT bump-only; THE RULING allows both and the hook's tests pin them.

Pending version, the one definition every row uses: the manifest version at HEAD (a) has no tag on the REMOTE, (b) is not below the latest local `v*` tag (no `v*` tags at all satisfies this), and (c) is not the Rust untouched default `0.1.0` while tags exist (that value defers to the tag, `main.rs` rule 1). A local-only tag for that version does not cancel pending: at HEAD it is the local-tag resume row, elsewhere it is the manual-surgery refusal that exists today. Per the 08-31 rule a pending version IS the release: nothing bumps past it, and `determine_version_action` (which bails "Version mismatch" on manifest 0.1.6 vs tag v0.1.5, `main.rs:193-215`) is never consulted while one exists. `compute_target_tag` runs only when the manifest equals the latest tag, is the untouched default, or the repo has no tags and the manifest is unset. A manifest BELOW the latest tag matches no release row and refuses by name: "manifest 0.1.4 is below the latest tag v0.1.5; bump never lowers a version, fix the manifest by hand".

Whose pending version it is decides the gated rows: the PACKAGE version line (as defined under Bump-only branch) changed between the merge base with `origin/<default>` and HEAD in `Cargo.toml pyproject.toml package.json` (Gate D's own test, `git-release-guard.sh:562-566`, scoped to the package version) means the branch bumped it; a pending version with no such line was inherited from the base. Comparing the manifest to the latest tag alone, as HEAD's `release.rs:401-423` does, cannot tell those apart and wrote a false `Release: rides` line for the inherited case. An inherited pending version is the one exception to "nothing bumps past it" (Scott, 2026-09-26: "bump again"): the branch bumps FROM the manifest version (`0.1.6` -> `0.1.7`), so the target is `bump_version(manifest, level)`, not `determine_version_action`, which would bail on the manifest-above-tag mismatch.

### API Design

```
bump release [-m|-M] [-n] [--install "<cmd>"|--no-install] [--standalone "<words>"] [--no-ci-gate] [--ci-timeout SECS]
bump finish  [-n] [--install "<cmd>"|--no-install] [--no-ci-gate] [--ci-timeout SECS]
```

`bump release` state table (rows that change vs the 2026-07-06 doc are marked NEW):

| State | Action |
|---|---|
| ungated, on default, clean, PENDING VERSION (manifest untagged), HEAD ahead of origin: a fix committed after red CI, or a version commit not yet pushed (a design doc's Phase 1 bumped it) | NEW: no version commit -> `git push --no-follow-tags origin <default>` -> confirm -> wait for green CI -> re-verify -> tag that version on the verified sha -> push tag by name -> install. A bare `bump release` takes the pending version; an explicit `-m`/`-M` implying a different one refuses naming both. Today this row bumps past the pending version or errors "version mismatch" (marquee v1.20.0, 2026-09-25) |
| ungated, on default, clean, PENDING VERSION, HEAD == origin (RESUME: a prior run died or CI was red and nothing new was committed) | confirm on origin -> NEW wait for green CI -> re-verify -> tag (on the verified sha) -> push tag -> install; never re-bumps |
| ungated, generic (no manifest), on default, clean, HEAD == origin, no remote tag at the tip | the generic RESUME row: the version lives in tags alone, so the untagged tip resumes the tag the level computes from the latest tag (or the latest local tag already at HEAD, unpushed); the re-verify skips only the manifest-version sub-check and keeps sha == tip, and a tip that moved during the wait refuses (audit round 1) |
| ungated, on default, clean, PENDING VERSION, HEAD == origin, a LOCAL tag for that version already at HEAD (a prior run died between tag and push) | confirm on origin -> wait for green CI -> re-verify -> push that tag -> install (HEAD's `resume_local_tag_present_pushes_only` keeps covering this) |
| any gate, manifest version below the latest tag | NEW refuse by name; bump never lowers a version |
| ungated, on default, clean, no pending version, ahead of origin | version commit -> `git push --no-follow-tags origin <default>` -> confirm on origin -> NEW wait for green CI -> re-verify -> tag the verified sha -> push tag by name -> install |
| ungated, on default, == origin, version tagged, `--standalone` | NEW: the fresh-release row; the version commit is the release. The version commit runs with `force` so the "HEAD already has a tag" guard (`main.rs:707`) does not refuse the tagged tip; scoped to the two standalone rows |
| ungated, on default, == origin, version tagged, no order | refuse "nothing to release", names `--standalone` |
| ungated, not on default / dirty / detached / gate unknown | refuse with the one exact fix (unchanged) |
| ungated, behind origin | refuse: `git pull --ff-only origin <default>` (unchanged) |
| ungated, diverged from origin (a rejected push, someone pushed first) | NEW refuse: `git pull --rebase origin <default>`, then re-run. The re-run classifies the rebased tree: a pending version rides the pending row; a version origin already tagged in the meantime is no longer pending and lands on "nothing to release" |
| gated, feature branch, a version line changed in the diff vs origin/default (the branch's own bump) | skip re-bump -> push -> ensure PR -> pause; no level flag reuses the riding version, an explicit level implying a different version refuses naming both |
| gated, feature branch carrying work, PENDING VERSION (same definition, so the untouched `0.1.0` and a no-tags repo do not trip it), NO version line in the diff (inherited from origin/default: a merge whose finish never ran, or went red) | NEW: bump again. Version commit from the manifest version (`1.21.0` -> `1.21.1`, or `-m`/`-M`), then the fresh row: push, PR with `Release: rides this PR (v1.21.1)`, pause. The inherited number is burned unless `bump finish` tags it before this PR merges; the verb prints that. Today this row skips the bump and writes a false `Release: rides` |
| gated, feature branch carrying work, no pending version, no version line in the diff | version commit (NEW: always a new commit) -> push branch -> probe open PR -> NEW create with derived title + body -> pause |
| gated, feature branch, diff vs origin/default empty or version-only, no order | NEW refuse: bump-only branch, names `--standalone` |
| gated, feature branch where `slug(branch) != branch` | NEW refuse before any mutation, prints `git branch -m <slug>` |
| gated, on default, clean, == origin, `--standalone` | NEW: `git checkout -b bump-vX-Y-Z --track origin/default` (or check out that branch if it already exists and classify it like any feature branch: it may carry the bump, carry work, or be empty) -> the gated fresh row with `force` on the version commit, body quotes Scott |
| gated, on default, clean, == origin, no order | refuse "bump rides a feature PR", names `--standalone` |
| gated, on default with commits not on origin | refuse with the literal rescue commands (unchanged) |
| gated, generic (no manifest) | refuse (unchanged) |

`bump finish` state table:

| State | Action |
|---|---|
| any worktree; origin/default carries an untagged version | NEW resolve the worktree holding default (own checkout, else `git worktree list`, else checkout here) -> compare its default to origin (Behind -> `pull --ff-only`; Equal -> nothing; Ahead or Diverged -> refuse before any pull, see below) -> load `bump.yml` there -> tag_ladder -> NEW wait for green CI on the merged sha -> re-verify -> tag that sha -> push tag by name -> install there |
| tag exists locally only at the merged tip | NEW wait for green CI -> push tag -> install |
| tag on the remote at the merged tip | "already released"; NEW still runs the install step, so a re-run after "tag pushed, install failed" installs (`--no-install` to skip) |
| tag for origin/default's version exists at a DIFFERENT commit, local or remote (bump never rode this merge) | refuse; NEW names the standalone door in the wording below |
| local default ahead of or diverged from origin | NEW refuse before any pull: ahead means local commits on the default branch that never landed (gated: they can only land via PR); diverged names `git pull --rebase origin <default>` then re-run. Today `pull_ff_only` runs first (`release.rs:836-844`) and a diverged default dies in git's own error before `tag_ladder`'s message is reached |
| tracked changes in the current or resolved worktree | refuse before anything moves |

CI gate (`wait_for_green`), every 15s, two reads per poll: `gh api repos/{slug}/commits/{sha}/check-runs?per_page=100` and `gh api repos/{slug}/commits/{sha}/status` (the legacy commit-status API).
- Any completed check run with a conclusion outside `success|skipped|neutral`, or a combined status of `failure`/`error` -> refuse, no tag; the message names the failed runs and says the re-run reuses the version.
- Legacy statuses are read off `total_count`, never off `state`: zero statuses report `state: "pending"` (observed on `scottidler/bump`), so keying on `state` would poll every zero-status repo to the timeout. A non-zero `total_count` with `state: pending` counts as incomplete and is subject to `--ci-timeout` like an incomplete check run.
- `total_count` larger than the runs returned (truncation), or any API/auth error from either read -> refuse, no tag. The gate never inherits the bash driver's error-to-empty fallback (`bin/release:205`).
- All runs completed and the combined status is `success` or has zero statuses -> proceed.
- Zero check runs and zero statuses after 120s: decided by whether the repo has CI at that sha, read mechanically as `git ls-tree <sha> .github/workflows` being non-empty. Has CI -> refuse: "CI never registered on <sha> after 120s; re-run when it has. If this repo's workflows never run on push, declare `ci: none` in bump.yml (committed, reviewed) and re-run." No CI -> proceed, printing that the repo has no workflows. A committed `ci: none` in `bump.yml` skips the gate with the same printed notice. The refusal never names `--no-ci-gate`: that flag stays for a human at a terminal, and a per-repo fact belongs in the repo's config, not in an agent's per-run decision (three org repos have workflows with no push trigger: pyspark-solutions-jobs, solutions-agent-interface, git-crypt; each gets one reviewed `bump.yml` line when it first releases through the verb).
- `--ci-timeout` elapsed with runs incomplete -> refuse, no tag.
- `Ok(None)` from the port (no GitHub remote) -> proceed. `--no-ci-gate` -> skip with a printed warning.
Inventory for the legacy API, observed 2026-09-26: `commits/<tip>/status` on `tatari-tv/marquee`, `tatari-tv/slack-cli` and `scottidler/bump` each report `total_count: 0`. Polling it costs one call and closes the class anyway. Inventory for external CI with no workflows tree (panel round 2, 445 local clones): none use `.circleci`, `.buildkite` or a `Jenkinsfile`; three old `.travis.yml` files exist, none active.

Tag placement and the two re-verifies: the tag is created on the explicit sha the gate verified, never on implicit HEAD (`git tag -a <tag> <sha>`). Before creating it, and again before pushing it, `origin/<default>` is fetched fresh and the sha must EQUAL its tip, and the manifest at that sha must carry the tag's version. Equality, not ancestry: marquee's release workflow promotes by waiting until `test` runs exactly the tagged sha (`tatari-tv/github-actions` `platform-promote-prod.yml:156`), so a tag on an older ancestor never promotes and cannot be taken back. If the tip moved during the wait and the manifest at the new tip still carries the same pending version, the CI gate restarts on the new tip and that tip is tagged; if the version at the new tip differs, refuse. The first re-verify runs before the tag exists, so a refusal there leaves no local tag; a tip that moves between the tag and the push (a force-push, or a merge that races the second fetch) refuses and leaves the local tag, and the refusal names the way out: re-run, and if origin/<default> no longer carries the tagged sha, `git tag -d <tag>` first. It fails safe, not self-healing: with the local tag off the new tip the re-run lands on the manual-surgery refusal or `Behind` (release) or `missed_bump` (finish), never on a silent retag.

Refusal wording for the two door rows and the finish missed-bump row: "If Scott already ordered a standalone release in this session, re-run with `--standalone "<his exact words>"`. Otherwise STOP and report; do not invent an order." Every other refusal names its one exact next command. No refusal says STOP without naming the door, so the 2026-09-08 / 09-26 "asserted the default without reading the exception" class has no text to misread, and none invites an agent to manufacture the order.

### Implementation Plan

Ship order: `scottidler/bump` first (Phases 0-7), then `scottidler/claude` (Phase 8), then the live gated finish (Phase 9). The claude-repo prose must not name `bump release` until the binary that has the flags is installed.

#### Phase 0: Prove the three mechanics with zero code
**Model:** sonnet
- `gh api repos/tatari-tv/marquee/commits/<sha>/check-runs?per_page=100` returns `check_runs[]` with `status` and `conclusion` per run. Observed 2026-09-26 on `b08db6c`: `total_count 8`, statuses `completed: 8`, conclusions `success: 7, skipped: 1`.
- `gh pr create` accepts `--head`, `--base`, `--title`, `--body` together. Observed 2026-09-26, gh 2.46.0, `gh help pr create` lines 32-48: `-B, --base branch`, `-b, --body string`, `-H, --head branch`, `-t, --title string`; `--dry-run` at line 36 for a zero-side-effect check later if wanted.
- `git branch -r --contains HEAD` is non-empty on a branch cut from `origin/main` with `--no-track`, and empty after one local commit. Observed 2026-09-26 in a scratch bare-remote clone: `contains after cut: [origin/main]`, `contains after local commit: []`.
- **Success criteria:** the three observations above are recorded (done).

#### Phase 1: git helpers and the amend fix
**Model:** sonnet
- `is_head_pushed`: remote-containment check (any remote-tracking ref, not only `origin/*`) before the `@{u}` check.
- New: `commit_subjects`, `changed_files`, `checkout_new_tracking`, `worktree_for_branch`.
- `Cli.never_amend` (`#[arg(skip)]`), honored at both amend sites in `process_directory`.
- **Success criteria:** a test cuts a branch from origin/main with no upstream and asserts `is_head_pushed == true`, then commits locally and asserts `false`; `bump --no-tag` on that fresh branch produces a new commit whose parent is `origin/main`.

#### Phase 2: github helpers
**Model:** sonnet
- `token_for_org` + `persona_token_var`; `gh_command` uses them.
- `CheckRuns` (with `statuses`), `check_runs_from_json`, `status_from_json`, `check_runs(path, sha)`: truncation (`total_count` > returned) and any non-success HTTP result are `Err`.
- `git::create_tag` gains a `sha` argument; `git::push_branch` gains `--no-follow-tags`; `git::remote_tip(dir, default)` fetches and returns the fresh tip, and `git::manifest_version_at(dir, sha)` reads the version via `git show <sha>:<manifest>`; together they answer the re-verify question.
- `config::Config` gains `ci: Option<CiDeclaration>` with the single variant `None`.
- The `create_pr` signature change lands in Phase 3 with its caller, so this phase stays green on its own.
- **Success criteria:** unit tests for `check_runs_from_json` (incomplete + failed counting, empty array, missing key is an error, `total_count` 101 with 100 returned is an error), `status_from_json`, and `persona_token_var`.

#### Phase 3: CI gate and PR by construction
**Model:** opus
- `Ci` port + `GhCi`; `Ports` gains `ci`; `release()` / `finish()` take it.
- `wait_for_green` per the API Design (both endpoints, `total_count`-keyed statuses, truncation and errors fail closed, the workflows-tree-at-sha decision on zero runs, `bump.yml` `ci: none`); wired into `execute_release`, `execute_resume`, both `finish` tag arms. The tag is created on the verified sha after a fresh fetch shows it EQUALS `origin/<default>` and the manifest at it carries the version, and pushed only after a second fresh fetch shows the same; a moved tip with the same pending version restarts the gate on the new tip.
- The pending-version state (Data Model definition, including the local-tag-at-HEAD resume row and the below-latest refusal), classified before the ahead/equal split on the ungated side and before the fresh/inherited split on the gated side; `compute_target_tag` is not called while one exists.
- `ReleaseOpts.bump_type: Option<BumpType>`; `None` takes the pending version or means patch on a fresh release. Lands here because this phase's tests need the bare re-run.
- `create_pr(path, branch, base, title, body) -> Result<String>` replaces `--fill`; `pr_title`, `pr_body`; `execute_gated` builds them and calls it; `ReleaseReport.pr_url`. The branch precondition `slug(branch) == branch` is checked in classification, before any mutation.
- Gated: the branch's own bump is the version-line-in-diff test; a pending version without that line is inherited and bumps again from the manifest (Scott's ruling).
- Test doubles: `NoCi` (None), `RedCi`, `GreenCi` (records the sha asked), `SilentCi` (zero runs, zero statuses), `TruncatedCi` (`total_count` > returned), `ErrCi`.
- **Success criteria:** `red_ci_leaves_no_tag_and_green_rerun_resumes_same_version`: red run leaves the version commit on origin untagged and no tag anywhere; the green re-run reports `resumed == true`, `tag == v0.1.6`, and `GreenCi` was asked exactly HEAD's sha. `gated_pr_title_and_body_are_built_from_branch_and_commits`: branch `add-thing` + subject `feat(core): add thing` -> title `feat(core): add thing`, base `main`, body ends with `Release: rides this PR (v0.1.6)`, and `HEAD~1` is still the feature commit. `inherited_pending_version_bumps_again`: main carries an untagged `0.1.6` over tag `v0.1.5`; a feature branch cut from it with one work commit gets a version commit to `0.1.7`, the body says `v0.1.7`, and the pause output names the untagged `v0.1.6`. `red_ci_then_fix_commit_tags_pending_version`: `0.1.6` pushed, `RedCi` refuses, a fix commit lands on main, a bare re-run pushes the fix and tags `v0.1.6` (never `v0.1.7`); `-m` on that tree refuses naming `v0.1.6` and `v0.2.0`. `truncated_or_erroring_ci_leaves_no_tag`. `silent_ci_refuses_when_workflows_exist_and_proceeds_when_none`: with `.github/workflows/ci.yml` tracked, `SilentCi` refuses; without it, proceeds. `tag_binds_to_verified_sha`: a `Ci` double that lands another commit carrying the same version on origin/main during the wait makes the gate restart on the new tip, and the tag lands on that tip, pushed; a double that lands a commit with a different manifest version refuses with no local tag created; a double that force-moves origin/main between the tag and the push refuses, leaves the local tag, and the re-run takes the local-tag resume row.

#### Phase 4: standalone, bump-only, bad branch name
**Model:** opus
- `ReleaseOpts.standalone`; empty words refuse.
- The standalone rows run the version commit with `force: true` (the tagged tip is the whole point); every other row keeps `force: false`.
- `classify_equal` (ungated): tagged + order -> `UngatedStandalone`.
- `classify_gated`: default clean + order -> `GatedStandalone`; feature branch: bad name -> `GatedBadBranchName`; `is_bump_only_branch` && no order -> `GatedBumpOnlyBranch`.
- `execute_gated_standalone`: `checkout_new_tracking(bump-vX-Y-Z, origin/default)` then the gated flow. If `bump-vX-Y-Z` already exists locally (a prior run died somewhere after cutting it), check it out and classify it like any feature branch from its actual diff: empty -> fresh standalone (with `force`); version line -> the branch's own bump, already bumped; work commits -> it proceeds as a feature branch, the bump rides with that work and Scott's words are still quoted (when that branch's PR is already open, its body was built by an earlier run, so the words go onto the PR as a `gh pr comment` instead). Existence alone proves nothing.
- `--standalone` on a branch that already carries work, or on an ungated default branch with commits ahead: accepted, the order is quoted in the PR body (gated; a comment on the PR when one is already open) or printed (ungated), nothing else changes.
- `UngatedPending { tag, default, ahead: bool }`: the manifest version at HEAD has no tag anywhere and is above the latest tag (not the Rust untouched default). Classified before the ahead/equal split. `ahead == false` is RESUME; `ahead == true` pushes first. An explicit level implying a different version refuses naming both.
- `GatedInheritedPending { branch, default, inherited: tag }`: gated feature branch, manifest untagged, no version line in the branch diff. Bumps again from the manifest version and prints "origin/<default> carries untagged vX.Y.Z; this PR releases as vX.Y.Z+1. To ship vX.Y.Z on its own first, run bump finish before merging this PR."
- `Behind` splits into `Behind` (message: `git pull --ff-only origin <default>`) and `Diverged` (message: `git pull --rebase origin <default>`, then re-run). Today both print the ff-only line, which cannot succeed on a diverged branch. The same split lands in `tag_ladder` (`main.rs:407-423`) for `finish`.
- The standalone PR title is `chore: bump v0 1 6` by construction (branch words). Ugly, and correct: the title must slugify to the branch, and PR #97 (`chore: bump v1 20 1`) already set the precedent.
- **Success criteria:** `gated_standalone_cuts_tracking_branch_bumps_and_quotes_scott`, from a tip that carries the `v0.1.5` tag: the recording pusher sees `@{u} == origin/main` at push time, after the push `@{u} == origin/bump-v0-1-6`, `HEAD~1 == origin/main`, PR body contains `Standalone release ordered by Scott: "release them to prod with a .1 release"`. `gated_bump_only_branch_refuses_without_standalone`: fresh-cut branch refuses with nothing bumped or pushed. `ungated_standalone_releases_from_tagged_default`: refuses without the order (message names `--standalone`), ships `v0.1.6` with it. `ungated_pending_version_is_pushed_and_tagged_not_rebumped`: main carries an unpushed commit that sets `0.1.6` over tag `v0.1.5`; a bare `bump release` (no level) pushes, tags `v0.1.6`, and the manifest still reads `0.1.6`; `bump release -m` on the same tree refuses naming `v0.1.6` and `v0.2.0`. `dep_bump_and_lockfile_only_branches_are_not_bump_only`: a branch changing one dependency line plus `Cargo.lock`, and a branch changing only `Cargo.lock`, both classify as fresh work. `zero_check_runs_refuses_with_workflows_and_proceeds_with_ci_none`: `.github/workflows/ci.yml` tracked at the sha and `SilentCi` -> refuse naming `bump.yml`; the same tree plus a COMMITTED `ci: none` in `bump.yml` -> proceeds with the notice; the same tree plus an UNTRACKED `bump.yml` carrying `ci: none` -> still refuses.

#### Phase 5: finish from any worktree
**Model:** opus
- `finish_dir`: own checkout -> sibling worktree via `worktree_for_branch` -> checkout here.
- Tracked-changes check on both the current and the resolved worktree; classify the resolved worktree's default against origin BEFORE any pull (Behind -> `pull --ff-only`; Ahead / Diverged -> refuse with their own wording); then `bump.yml`, ladder, CI gate, re-verify, tag on the verified sha, re-verify, push, install, all in the resolved one.
- The already-released row runs the install step, so "tag pushed, install failed" is recovered by re-running `bump finish`.
- **Success criteria:** `finish_from_feature_worktree_finishes_in_the_default_worktree`: `git worktree add` a feature worktree, run `finish` from it, assert the main worktree fast-forwarded to `0.1.6`, the tag is on `origin/main`'s tip and pushed, and the feature worktree is untouched. `finish_red_ci_leaves_no_tag`. `finish_already_released_still_installs`. `finish_diverged_default_refuses_before_pull`: the resolved worktree's main has a local commit origin lacks and origin has moved; finish refuses naming `git pull --rebase`, and the worktree is untouched.

#### Phase 6: CLI surface and docs
**Model:** sonnet
- Flags per API Design; `bump release --help` / `bump finish --help` after-help rewritten to the two flows, the refusals, RESUME, `--standalone`, and the persona token rule.
- Default install: `cargo install --path .` only when the root `Cargo.toml` has a `[package]` table. A virtual workspace root (`tatari-tv/marquee`: `[workspace]` with members, no root package) has no default install; the verb prints "install: skipped (virtual workspace root; pass --install or set install: in bump.yml)". On main today `resolve_install` returns the default whenever `Cargo.toml` exists, and `cargo install --path .` fails on a virtual manifest, which would fail the release after the tag is pushed.
- README release section rewritten to the tables above; implementation notes appended.
- **Success criteria:** clap parse tests assert `--standalone`, `--no-ci-gate`, `--ci-timeout` and the absent level reach `ReleaseOpts` as `Some(words)`, `ci_gate == false`, the given `Duration`, and `bump_type == None`; `bump release -n --no-ci-gate` on a fixture prints the `CI gate: SKIPPED` line, proving the flag reaches execution; `otto ci` green (clippy `-D warnings`, fmt, em-dash lint).

#### Phase 7: ship bump with itself
**Model:** sonnet
- `cargo install --path .` the new binary, then from `main` in `scottidler/bump` (ungated) run `bump release -m` as a bare command. It must: commit the version, push, run the CI gate, tag, push the tag, install.
- `scottidler/bump` has no `.github/workflows/` (observed 2026-09-26), so the gate finds zero check runs and proceeds after the 120s appear window. That is the no-CI row exercised live, on purpose; `--no-ci-gate` is not used here.
- **Success criteria:** `git ls-remote origin 'refs/tags/<tag>^{}'` resolves to the same sha as `git rev-parse origin/main`; `bump --version` reports the tag; `~/.local/share/bump/logs/bump.log` shows `wait_for_green` logged before `create_tag` for that run, with the no-workflows decision printed.

#### Phase 8: retire the bash driver and the door (operator step, `scottidler/claude`)
**Model:** opus
- First bullet, a gate: `bump --version` reports the Phase 7 tag or newer; otherwise stop.
- `git-release-guard.sh`: `bump release` / `bump finish` skip the legacy bump gates (the verb enforces its own invariants); delete the `BUMP_ORDERED_BY_SCOTT` door and every message that says STOP without naming `bump release --standalone`; Gate D exempts `gh pr create --help` / `-h` (denied live 2026-09-26 on a help lookup); header rewritten to the two verbs. `git-release-guard-test.sh`: door cases replaced by `bump release` allowed on a feature branch and on main, `bump finish` allowed with an untracked file, `gh pr create --help` allowed; raw-git denials unchanged.
- `git rm HOME/.claude/bin/release`; drop its `manifest.yml` `release-on-path` block (`:79-85`) and remove the `~/bin/release` symlink it created; drop `settings.json` `Bash(release:*)` allow + `release *` excludedCommands entries. `pr-open` stays; its header's `bin/release` history paragraph updated.
- `/bump` SKILL.md, `/shipit` SKILL.md, `/babysit` SKILL.md (`:115-118` names `release --finish`), `release-driver.md` (including the `run_in_background` contract for the two verbs), `rules/git.md` release section, `/how-to-execute-a-plan` release section, `panel-round-guard.sh:35` comment, `hooks/fixtures/inline-token/fixtures.json`: rewritten to `bump release` / `bump finish` / `--standalone`, run as bare commands after a separate `cd`. No mention of `bin/release`, `pr-open --release rides`, `release --pr`, `release --finish`, or the env-var door anywhere except `~/HALL-OF-SHAME.md` history.
- `~/HALL-OF-SHAME.md`: dated amendment under THE RULING: the door is `--standalone`, the bash driver is gone, the hand-back is gone.
- Baton log entry in `2026-09-13-setup-audit-program.md`.
- **Success criteria:** the three Acceptance Criteria below that name the hook, the stale references and `bin/release`, each run verbatim.

#### Phase 9a: live gated release
**Model:** sonnet
- The first feature PR on any gated `tatari-tv/*` repo after Phase 8 ships is opened by `bump release` run bare by the `release-driver` agent under auto mode. Record here the repo, the PR url, the session id, and that no `[Auto-Mode Bypass]` / `[Production Deploy]` denial fired.
- **Success criteria:** the PR body's last line is `Release: rides this PR (vX.Y.Z)` and `gh pr view <url> --json title,headRefName` satisfies `title_slug(title) == headRefName`.

#### Phase 9b: live gated finish
**Model:** sonnet
- `tatari-tv/marquee` `origin/main` is at `1.21.0`, untagged (PR #98 merged with its bump; `v1.20.1` is the last tag). From the marquee feature worktree, run `bump finish --install "cargo install --path cli"` as a bare command (marquee's README names that install; the root is a virtual workspace). It must resolve `~/repos/tatari-tv/marquee`, pull, wait for CI on the merged sha, tag `v1.21.0`, push it, install the CLI.
- Scott runs or orders this one: a `v*` tag promotes to prod.
- **Success criteria:** `sdv probe marquee.internal.tatari.dev | jq -r '.version.payload | "\(.version) \(.revision)"'` reports `v1.21.0` and the sha of `v1.21.0^{commit}`.

## Acceptance Criteria

- [ ] `bump release --help | grep -oE -- '--(standalone|no-ci-gate|ci-timeout)' | sort -u | wc -l` prints `3` (presence of each flag, not a line count -- the after-help prose names them literally, so a line-count form miscounts the moment prose mentions a flag more than once).
  Observed on main (installed v0.3.3): `0`.
  Observed on branch `one-release-command` (Phase 6 follow-up commit, `cargo run -q -- release --help | grep -oE -- '--(standalone|no-ci-gate|ci-timeout)' | sort -u | wc -l`): `3`.
- [ ] `test ! -e ~/.claude/bin/release && test ! -L ~/.claude/bin/release && test ! -L ~/bin/release` exits 0.
  Observed on main: `~/.claude/bin/release` exists (`rc=0` from `test -e`) and `~/bin/release` is a symlink to it.
- [ ] `grep -c BUMP_ORDERED_BY_SCOTT ~/.claude/hooks/git-release-guard.sh` prints `0`.
  Observed on main: `2`.
- [ ] `rg -l 'bin/release|pr-open --release|release --finish|release --pr|BUMP_ORDERED_BY_SCOTT' ~/repos/scottidler/claude/HOME/.claude/skills ~/repos/scottidler/claude/HOME/.claude/agents ~/repos/.claude/rules ~/repos/scottidler/claude/HOME/.claude/hooks/git-release-guard.sh ~/repos/scottidler/claude/HOME/.claude/hooks/git-release-guard-test.sh ~/repos/scottidler/claude/HOME/.claude/hooks/fixtures ~/repos/scottidler/claude/manifest.yml | wc -l` prints `0`.
  Observed on main: `9` (the seven from the first count plus `git-release-guard-test.sh` and `fixtures/inline-token/fixtures.json`).
- [ ] `~/.claude/hooks/git-release-guard.sh --self-test` exits 0 and its last line reports `fail=0` with `pass` >= 269.
  Observed on main: `pass=269 fail=0`.
- [ ] `git -C ~/repos/scottidler/bump grep -c -E 'fn (red_ci_leaves_no_tag|gated_standalone_cuts|finish_from_feature_worktree)' HEAD -- src/release/tests/ | awk -F: '{s+=$NF} END {print s}'` prints `3` and `cargo test` in that repo is green. (Amended from a single-file `show HEAD:src/release/tests.rs` grep: the module split at `46aed56` moved these three tests into `src/release/tests/{gate,standalone,finish}.rs` under the 1500-line cap, `rules/rust.md:113`; the original form now greps a path that no longer holds them.)
  Observed on main (`77ad42e`): `0`.
  Observed on branch `one-release-command` (`ed8640c`): `3` (`gate.rs:9`, `standalone.rs:57`, `finish.rs:310`).
- [ ] A gated release from a feature branch (Phase 9a) and a gated finish from a sibling worktree (Phase 9b) each complete as ONE bare `bump ...` Bash call by the `release-driver` agent under auto mode, with no `[Auto-Mode Bypass]` or `[Production Deploy]` denial. Cannot run until Phases 7-8 ship; recorded then, with the session id.

## Resolved Decisions

- 2026-09-26, Scott: two flows, decided by whether `main` is protected. The verbs follow `bump --gates`. (His message: "1. tagging repos that protect main, require a PR 2. tagging repos that DO NOT protect main, DO NOT require a PR")
- 2026-07-03, Scott (THE RULING): the bump rides the feature PR; a bump-only branch is forbidden; the tag is cut on main after the merge. Carried into the verb as the `GatedBumpOnlyBranch` refusal.
- 2026-07-10, Scott: an explicit standalone order is the exception and re-asking is a violation. Carried as `--standalone "<words>"`, words quoted into the PR body.
- 2026-08-31, Scott (otto double-tap): no tag before green CI on the exact sha. Carried as the CI gate in both verbs.
- 2026-07-06 panel: stranded commits on a gated default branch refuse with literal rescue commands rather than auto-rescue. Kept.
- This doc: the release verb's version commit is always its own commit. Plain `bump --no-tag` keeps its amend-when-unpushed behavior, minus the false negative on a branch cut from origin.
- This doc: PR creation moves inside `bump release`. The 2026-09-17 reason for the hand-back (PreToolUse must see `gh pr create`) is met by porting the two checks the gates run: the verb refuses a branch that is not its own slug (the title guard's test) and decides "already bumped" by the version line in the diff against the base (Gate D's test). Panel round 1 showed the first draft's "by construction" claim was false on both counts without those ports. Gate D and the title guard still apply to any raw `gh pr create`.
- 2026-09-26, Scott: "bump again". On a gated feature branch that inherited an untagged version from the default branch, the verb bumps from the manifest version and the PR carries its own release. The panel's round 2 objection (the inherited number is burned when nobody runs `bump finish` first) and the author's round 2 disposition (refuse and name two paths) are recorded and overridden. Ungated pending versions are unaffected: on the default branch the pending version is still the release.
- 2026-09-26, Scott ("A"): the tag goes on the newest commit on the default branch at tagging time, never on an older one. If main moved during the CI wait and the version is unchanged, the CI wait runs again on the new tip and that tip is tagged; if the version changed, stop and print why. In code: the re-verify invariant is EQUALITY with a freshly fetched origin/<default> plus "the manifest at the sha carries the tag version", checked before the tag is created and before it is pushed. Ancestry was round 2's refinement; marquee's promote gate (`platform-promote-prod.yml:156`) waits for the exact tagged sha, so an ancestor tag never promotes. A tip that moved but still carries the same pending version restarts the CI gate on the new tip; a changed version refuses.
- 2026-09-26, panel round 1, author's disposition: the CI gate does not fail open on zero runs and does not fail closed unconditionally. The repo's `.github/workflows` tree at the sha decides; a repo whose workflows never run on push declares `ci: none` in its committed `bump.yml`; the legacy status API is polled alongside check runs; truncation and API errors refuse. Rationale for rejecting unconditional fail-closed: it turns every no-CI release into a `--no-ci-gate` decision for the agent, and an agent flag-to-get-unstuck is the failure class this doc exists to remove. Round 2 accepted the shape with the `bump.yml` declaration replacing the flag hint.

## Alternatives Considered

### Alternative 1: keep the bash driver and fix the door
- **Description:** make `bin/release` do the PR too; move the door to a file marker the hook reads.
- **Pros:** no Rust work.
- **Cons:** keeps two drivers; the bash driver scrapes `bump -n` stdout for version numbers; the door stays a side channel the classifier cannot see.
- **Why not chosen:** the audit baton already named two drivers as the defect; the Rust verb has typed state and tests.

### Alternative 2: keep the hand-back so PreToolUse inspects `gh pr create`
- **Description:** `bump release` stops at "PR creation required" and prints the `gh pr create` line.
- **Pros:** the gates see the command.
- **Cons:** three more agent-run steps; every 09-2026 incident lives in those steps; `pr-open` refuses dotted names, `release --pr` re-verifies, all recoverable only by an agent doing the right thing.
- **Why not chosen:** the gates exist to catch a wrong PR; a PR built by the tool cannot be wrong on the two axes they check.

### Alternative 3: CI-minted tags (tag on merge in a workflow)
- **Description:** a GitHub workflow tags the merged commit when the version line changes.
- **Pros:** no local finish step.
- **Cons:** rejected 2026-07-06 (Alternative 2 there): moves the irreversible step out of Scott's hands and away from the install/probe half.
- **Why not chosen:** unchanged ruling.

### Alternative 4: env-var door kept, allow rules widened
- **Description:** add `Bash(BUMP_ORDERED_BY_SCOTT=1 bump:*)`-style allow rules.
- **Pros:** no hook change.
- **Cons:** the classifier still sees an env prefix on an excluded command; rules would multiply per command shape (`git checkout -b`, `git push`, `gh pr create`).
- **Why not chosen:** a flag on one verb replaces N rule shapes.

## Technical Considerations

### Dependencies
- `gh` >= 2.0 for `api`, `pr list`, `pr create --title --body --head --base`; `git` >= 2.20 for `worktree list --porcelain`, `branch -r --contains`.
- `GITHUB_PAT_WORK` / `GITHUB_PAT_HOME` in the environment (they are: dotfiles `.zshenv`). Absent, ambient `gh auth` applies, as today.
- No new crates: `serde_json` (already a dependency) parses check-runs.

### Performance
- CI gate polls every 15s, up to 1800s by default. A repo with no CI costs 120s once (the appear window) per tag. `--no-ci-gate` for repos known to have none.

### Security
- Tokens ride the established env channel; bump never reads secrets files except the pre-existing `~/.config/github/tokens/<org>` convention. Tokens are set on the subprocess env only, never printed or logged (debug logs name the variable, not the value).
- `--standalone` is transcript-visible on the command and quoted in the PR body, the same audit trail the door had.

### Testing Strategy
- Real git against bare local remotes, `BUMP_GATES_PROBE` forcing the gate verdict, `Pr` / `Ci` / `Pusher` / `Installer` doubles. Every new state has a test that asserts the mutation did or did not happen (tags, pushes, commits), not the message alone.
- Break-the-code proof for the two invariants: remove the `wait_for_green` call before `create_tag` and `red_ci_leaves_no_tag...` must fail; remove the `never_amend` short-circuit and `gated_pr_title_and_body...` must fail on the `HEAD~1` assertion.
- Hook: `git-release-guard-test.sh` matrix extended, run via `--self-test`.

### Rollout Plan
- Phase 7 releases bump with the new verb (dogfood, ungated).
- Phase 8 lands in `scottidler/claude` (ungated, symlinked live by manifest) after Phase 7's binary is installed, so no prose names a flag the installed binary lacks.
- Phase 9 is the first gated finish; Scott's call to run it.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| The classifier still denies a bare `bump release` as `[Production Deploy]` | Med | High | `Bash(bump:*)` is an explicit allow rule today; the acceptance criterion records the live run. If denied, the remedy is a rule, recorded in the doc, never a wrapper |
| A repo with no workflows pays the 120s appear window per tag | Low | Low | accepted: the no-CI decision is read from the tree, so no flag is needed; `--no-ci-gate` remains for the impatient |
| `gh pr create` needs the branch on origin before it runs | Low | Med | the verb pushes first, same order as today |
| `worktree_for_branch` finds a worktree with tracked changes | Low | Med | refused before `pull --ff-only`, with the path named |
| Legacy `bump --no-tag` callers relied on the amend on a fresh branch | Low | Low | only the false negative is fixed; a truly unpushed HEAD still amends |
| Phase 8 prose lands before Phase 7's binary is installed | Low | Med | ship order stated; Phase 8's first bullet is `bump --version` >= the Phase 7 tag |
| A repo's CI registers check runs later than 120s after the push | Low | Med | with workflows present the gate refuses rather than proceeding; the re-run reuses the version. The appear window is a constant, raised if it bites |
| Rejected `git push origin main` (someone pushed first) leaves the version commit local; `pull --ff-only` cannot apply | Low | Low | the refusal names `git pull --rebase origin main`; the re-run hits the already-bumped row and tags the same version instead of bumping again |
| Install fails after the tag is pushed (e.g. a virtual workspace root) | Low | Med | Phase 6 skips the default on a virtual root; an install failure is reported, the release is not rolled back (the tag is public) |

## Open Questions

- None. Rounds 1-3 are folded; the panel cap is reached. Scott ruled on both open decisions on 2026-09-26: inherited pending version bumps again; the tag goes on the newest commit on the default branch (option A).

## References

- `~/HALL-OF-SHAME.md`: THE RULING (2026-07-03), the door (2026-07-10), otto double-tap (2026-08-31), entries 2026-09-08, 09-15, 09-24, 09-25, 09-26.
- `scottidler/bump/docs/design/2026-07-06-release-verbs-and-language-adapters.md` (the verbs' first design; this doc supersedes its state tables).
- `scottidler/claude/docs/design/2026-09-13-setup-audit-program.md:82,91` (two drivers, handed to bump).
- `scottidler/claude/docs/design/2026-09-17-dm-resolution-and-pipeline-glue.md` Phases 5-11 (the hand-back and `pr-open`, retired here).
- `~/repos/.claude/rules/git.md`, `general.md` "Branch names".
