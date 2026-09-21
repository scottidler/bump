# CLI Shakedown Report: bump v0.3.2

Installed binary: `/home/saidler/.cargo/bin/bump`, `bump --version` -> `bump v0.3.2`.

## Summary

| Metric | Count |
|---|---|
| Commands discovered | 3 (`bump`, `bump release`, `bump finish`) + `--gates`/`--tag-only`/`--no-tag` modes |
| Commands tested | 9 distinct flows, real invocations |
| Commands passed | 7 |
| Commands failed (real bugs) | 2 |
| Edge cases tested | 6 |
| Scratch repos used | 2 (Cargo fixture + real `git worktree`, generic no-manifest fixture) |

`bump` has no `--json`/`--csv` output; it is a git-mutation CLI, not a data-query tool, so the Output Format Matrix phase does not apply.

## Command Results

**Read-only, real repo (`~/repos/scottidler/bump`):**
- `bump --gates` -> `Gates: none (ungated)`, correct.
- `bump -n` -> previews `0.3.2 -> 0.3.3`, correct.
- `bump release -n` -> exit 1, `nothing to release: nothing ahead of origin/main and the version is already tagged`, correct.
- `bump finish -n` -> full 6-step dry-run plan, correct.

**Mutating, scratch repo (bare origin + Cargo fixture):**
- `bump -m --automatic` on a clean tree -> amended commit, tagged `v0.1.0`. Correct.
- `bump -m --no-tag --automatic` -> version-only commit, no tag, correct hint to run `bump --tag-only` after merge.
- `bump --tag-only` with an **untracked** stray file present, on `main` -> **succeeded**, tag created, stray file untouched (`?? stray-untracked.txt` still shown after). This is the exact rung-1 bug just fixed, verified live against the shipped v0.3.2 binary, not just the unit test.
- `bump --tag-only` re-run on the same already-correctly-tagged HEAD (annotated tag pushed to origin) -> **failed**, see Bug 1 below.
- `bump -m` (no changes, HEAD already tagged, no `--force`) -> correctly refused: `HEAD already has a tag. ... Use --force to override.`
- `bump -m --force --message "forced release"` (HEAD pushed) -> succeeded, new commit created **with** the custom message. Correct on this path; see Bug 2 for the other path.
- `bump -m` on a **generic** (no manifest) repo, fresh -> `v0.1.0` created correctly, tag-based versioning works with no Cargo/pyproject/package.json.
- `bump --tag-only` on that generic repo -> failed with a malformed message, see Bug 3.

**Worktree independence (the second shipped fix):** a real `git worktree add -b other-worktree-branch` at the exact merged commit could not be exercised end-to-end for `--tag-only` in this session: a separate Claude-Code `PreToolUse` hook (in `scottidler/claude`, not this repo) blocks any `bump --tag-only`/`finish` invocation whose target worktree's branch name isn't the configured default, regardless of whether the commit is safe. That hook is heuristic (command/branch-name based) and was explicitly called out as "correct, no change wanted" in this session's earlier handoff, so I did not attempt to route around it. The fix is proven instead by the `tag_only_succeeds_on_differently_named_branch_at_merged_head` unit test (`src/main.rs`), which exercises the identical condition (current branch != default, HEAD == origin/default) without going through that hook.

## Failures & Bugs

### Bug 1 (real, reproducible): `--tag-only`/`finish` idempotency broken for every annotated remote tag
- **Severity:** bug (breaks a documented, load-bearing behavior: re-running `--tag-only`/`finish` after a successful release should be a silent no-op).
- **Where:** `src/git.rs:373-401`, `remote_tag_sha`. It queries `git ls-remote origin refs/tags/<tag>` with a **single, exact** refspec. Verified empirically: an exact-refspec query never returns the peeled `^{}` commit line (only the plain line, i.e. the annotated tag's own object SHA); the peeled line only appears if the peeled refspec is *also* requested, exactly as `remote_tag_commit` (a few lines below, `git.rs:414-441`) already does correctly by passing both `&refspec` and `&peeled` to the same `ls-remote` call.
- **Consequence:** `tag_ladder`'s classification (`main.rs`) compares this un-peeled tag-object SHA to `head` (a commit SHA). Since `bump` only ever creates *annotated* tags, this comparison can never be true, so `TagState::RemoteAtHead` is effectively dead code and every re-run of `--tag-only`/`finish` against an already-correctly-tagged commit misclassifies as `RemoteAtOther` and refuses with a **wrong SHA** in the error (the tag object's SHA, not the commit it points to). `finish()` happens to dodge this because its `RemoteAtHead | RemoteAtOther` arm re-resolves via `remote_tag_commit` before deciding (`release.rs`); `tag_only()` in `main.rs` does not, and is directly broken by it.
- **Reproduced twice:** once against the real `bump` repo (re-running `--tag-only` on the just-shipped `v0.3.2` falsely said "not HEAD"), once in a minimal from-scratch fixture (fresh bare repo, one tag, immediate re-run of `--tag-only`).
- **Fix shape:** make `remote_tag_sha` request both the exact and peeled refspecs in one `ls-remote` call, same as `remote_tag_commit` already does (or just have `tag_ladder` call `remote_tag_commit` instead of `remote_tag_sha` for this comparison).
- **No unit test currently catches this**: `tag_only_idempotent_local_tag_at_head` only covers the LOCAL-tag idempotency path; there is no `..._remote_..._at_head` equivalent.

### Bug 2 (minor, silent): `--message`/`--automatic` ignored on the amend path
- **Severity:** cosmetic-to-moderate (silently ignoring a flag the user explicitly passed, no warning).
- **Where:** `src/main.rs` clean-tree workflow, "HEAD is not pushed" branch (~line 749): calls `git::amend_commit_no_edit` (keeps the OLD message) and hardcodes the tag message to `"Bump version to {new_tag}"`. `cli.message` / `cli.automatic` are only read by `determine_commit_message`, which is only called on the "HEAD is pushed -> new commit" branch.
- **Repro:** on an unpushed HEAD, `bump -m --message "custom release note"` produced a commit still titled from the prior, unrelated commit ("pre-force commit"), not "custom release note".
- **`--help` does not document this caveat**, so the flag silently no-ops depending on push state the user may not be tracking.

### Bug 3 (cosmetic): malformed error message on generic + `--tag-only`
- **Severity:** cosmetic.
- **Where:** `src/lang.rs:222-228`, `version_file_name(ProjectType::Generic)` returns `""`.
- **Repro:** `bump --tag-only` on a manifest-less repo -> `Error: --tag-only needs a version in ; none found. (Generic/tag-only projects have no manifest version to tag.)` -- note the empty `in ;`.
- **Fix shape:** return something like `"(no manifest)"` for `Generic`, or special-case the message when the file name is empty.

## Edge Cases

| Input | Result |
|---|---|
| `--tag-only --no-tag` together | clap refuses: `cannot be used with '--no-tag'`, exit 2. Correct. |
| `--major --minor` together | clap refuses: `cannot be used with '--minor'`, exit 2. Correct. |
| Nonexistent directory arg | `Error: Not a git repository: /nonexistent/path`, exit 1. Correct, no panic. |
| `release --no-install` used on plain `bump` (not `release`/`finish`) | clap: `unexpected argument '--no-install'` with a helpful "did you mean '--no-tag'" tip, exit 2. Correct. |
| `bump release` / `bump release --no-verify` against a non-GitHub remote | both fail closed: `gate status is UNKNOWN ... refuses to guess`. `--no-verify` is documented as a top-level flag but has no effect on `release`'s gate check (by design per its own doc comment: release always fails closed on an unknown verdict); worth a `--help` footnote since it reads as an override that silently isn't one for this subcommand. |
| `bump -m` with HEAD already tagged, no `--force` | correct refusal naming the exact override flag. |

## Pipeline Recipes

Not applicable: `bump` has no query/list output to pipe; every real command mutates git state directly. The documented recipes in `bump --help` (ungated/gated flows) were exercised individually above rather than chained, since chaining them for real means an actual push/PR cycle against a hosted remote.

## Observations

- Help text is unusually good: every subcommand embeds its own state table, required-tool version check, and log path. No gaps found in discoverability.
- `bump release`'s fail-closed-on-unknown-gate behavior is a deliberate, correct design choice (documented), not a bug, but the local scratch harness could not exercise `release`'s ungated-happy-path or any gated flow without a real GitHub-hosted remote; that part of `release`/`finish`'s PR-integration behavior is untested by this shakedown.
- Two unrelated Claude-Code `PreToolUse` hooks (dirty-tree-before-bump, wrong-worktree-before-tag) fired repeatedly during scratch testing. Both are environment policy, not `bump` bugs, and both are already known/intentional per this session's earlier work.
