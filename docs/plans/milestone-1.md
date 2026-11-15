# Milestone 1: a daily-driver Git client

Status: Draft for review
Date: 2026-11-15

## Goal

Ship a signed macOS build of the Rust + GPUI Bonsai that someone can use as
their only Git client for ordinary work, connect GitHub, GitLab and Bitbucket
accounts, and purge a leaked file from history in a few clicks.

The SwiftUI app on `stable` is the feature reference, not the target. Its ~300
specs chased Fork parity one control at a time. This milestone instead picks
the workflows people need every day, makes each one complete and safe, and adds
the few things that make Bonsai worth switching to.

Architecture is in [ADR 1](../adr/0001-rust-gpui-gpuicn.md) to
[ADR 5](../adr/0005-app-architecture.md). Terms follow [CONTEXT.md](../../CONTEXT.md).

## What makes it more than a Fork clone

- **Purge a file from history**: pick any path that ever existed (even one
  deleted since), see every commit and branch it touches, confirm, and Bonsai
  rewrites history with a safety backup, then guides the force-push and reminds
  you to rotate the secret. ([ADR 4](../adr/0004-purge-engine.md))
- **Providers built in**: sign in to GitHub, GitLab and Bitbucket, browse and
  search your remote repositories, clone with one click, and publish a local
  repository that has no remote yet.
- **Everything is a command**: every operation is in the command palette, the
  menu bar and a key binding from one table.
- **Nothing hidden**: an activity log shows the exact Git commands each
  operation ran, with output, so errors are explainable.
- **Safe by default**: one confirmation pattern for destructive operations,
  safety backups before rewrites, and force-push only ever with lease.

## In scope

| Area | Included in milestone 1 |
| --- | --- |
| App shell | Single window with repository tabs, sidebar, resizable panes, light and dark themes, native menu bar, command palette, restored layout |
| Repositories | Open, drag in, init (no remote needed), clone from URL with progress, recents |
| Changes | Staged, unstaged, untracked and conflicted lists; stage, unstage, discard and ignore files; stage and discard hunks and lines |
| Commit | Message composer, amend, respects signing config and hooks, shows hook output, recent messages |
| Diff | Unified and side-by-side, syntax highlighting, whitespace toggle, binary and too-large states |
| History | Virtualized commit graph with branch and tag labels, commit details, changed files and diff, search by message, author, hash and path |
| Branches and tags | Create, rename, delete, checkout (including remote-tracking), upstream with ahead and behind counts, lightweight and annotated tags |
| Remotes | Fetch, pull (merge or rebase), push, publish branch, force push with lease, add, edit and remove remotes, clear auth errors |
| Integrate | Merge, rebase onto, cherry-pick, revert, reset (soft, mixed, hard); in-progress banner with continue, skip and abort; basic conflict handling (take ours or theirs, mark resolved, open in editor) |
| Stash | Save (with untracked), apply, pop, drop, view |
| Providers | GitHub, GitLab and Bitbucket Cloud accounts; browse and search remote repositories; clone; publish a local repository; any other Git server as a plain remote |
| Purge | Full purge flow with history path search, preview, safety backup, verification, force-push assistant and restore from backup |
| Activity log | Per-repository log of operations and commands |
| Ship | Settings, performance budgets on large repositories, signed and notarized DMG |

## Deferred to later milestones

Listed so they stay visible, roughly in the order they would come next:

- Interactive rebase (reorder, squash, edit, reword) and a three-way merge
  editor (GPUI Kit's diff already has conflict decorations to build on).
- Undo for any operation from the reflog, and a reflog browser.
- Scan history for secrets and large files, feeding straight into purge.
- Blame, file history and line history.
- Pull requests, issues, releases and notifications on providers.
- Worktrees, submodules, Git LFS, git-flow, bisect.
- Image diffs and a file tree browser at any commit.
- GitHub Enterprise Server and self-managed GitLab (the data model already
  stores a base URL per account).
- AI commit messages and branch review (the SwiftUI app had these).
- Linux and Windows builds.

## Issue breakdown

Issues are listed in build order. "Needs" names the issues that must land
first; issues without a shared dependency can run in parallel. Labels follow
[docs/agents/triage-labels.md](../agents/triage-labels.md). gpuicn issues are
filed in karyalabs/gpuicn, because reusable UI ships there first (ADR 1).

### Phase 0: foundation

| # | Issue | Needs | Label |
| --- | --- | --- | --- |
| B1 | Scaffold the Cargo workspace with gpuicn in crate mode, a launchable window, `make check`, and macOS CI | none | ready-for-agent |
| B2 | `bonsai-git`: Git process runner (environment, askpass hook point, cancellation, version check, command log) and repository discover, open and init | B1 | ready-for-agent |
| B3 | `bonsai-git`: repository snapshot (status via porcelain v2, HEAD, refs, upstreams, remotes, stashes, in-progress operation) with fixture tests | B2 | ready-for-agent |
| B4 | App: `RepositoryModel` entity, per-repository operation queue, FSEvents watcher with debounce | B3 | ready-for-agent |
| B5 | App shell: sidebar, resizable panes, repository tabs, native menu bar, command palette, one action table for keys, menus and palette | B1 | ready-for-agent |
| B6 | Repository manager: welcome screen, open, drag in, init, clone from URL with progress, recents | B4, B5 | ready-for-agent |

### Phase 1: everyday Git

| # | Issue | Needs | Label |
| --- | --- | --- | --- |
| G1 | gpuicn: Diff component on GPUI Kit's `diff` (unified and split, syntax highlighting, hunk and line selection intents, read-only mode) | none | ready-for-agent |
| G2 | gpuicn: Commit graph component aligned with Virtual List rows, drawing caller-supplied lanes, nodes, edges and labels | none | ready-for-agent |
| G3 | gpuicn: Tree component on GPUI Kit's `tree` (used for the changes tree mode and the purge path picker) | none | ready-for-agent |
| B7 | Changes view: grouped lists, stage, unstage, discard and ignore files, list and tree modes | B4, B5, G3 | ready-for-agent |
| B8 | Diff pipeline in `bonsai-git` (work tree, index, commit and range diffs; binary and size limits) and the diff pane | B3, G1 | ready-for-agent |
| B9 | Stage, unstage and discard hunks and lines | B7, B8 | ready-for-agent |
| B10 | Commit composer: amend, signing from config, hook output, recent messages, empty and failed states | B7 | ready-for-agent |
| B11 | `bonsai-graph`: lane layout plus paged history loading through gix with a CLI fallback | B3 | ready-for-agent |
| B12 | History view: graph, ref labels, commit details, changed files, diff, search and filter | B8, B11, G2 | ready-for-agent |
| B13 | Branches and tags in the sidebar: create, rename, delete, checkout, upstream and ahead/behind, tags | B4, B5 | ready-for-agent |
| B14 | Remotes and sync: fetch, pull, push, publish branch, force push with lease, remote management, progress and auth errors | B13 | ready-for-agent |
| B15 | Merge, rebase onto, cherry-pick, revert, reset; in-progress banner; basic conflict handling | B12, B13 | ready-for-agent |
| B16 | Stashes: save with untracked, apply, pop, drop, view | B8, B13 | ready-for-agent |
| B17 | Activity log panel and a shared error and confirmation pattern | B4 | ready-for-agent |

### Phase 2: providers

| # | Issue | Needs | Label |
| --- | --- | --- | --- |
| B18 | Register OAuth apps for GitHub, GitLab and Bitbucket and record the client IDs | none | ready-for-human |
| B19 | `bonsai-keychain`: Keychain token store and the askpass entry point used by the Git runner | B2 | ready-for-agent |
| B20 | `bonsai-providers`: `Provider` trait, `HttpClient` seam, GitHub (device flow, user, list and search repositories, create repository) | B18, B19 | ready-for-agent |
| B21 | GitLab provider (OAuth with PKCE or personal access token) | B20 | ready-for-agent |
| B22 | Bitbucket Cloud provider (OAuth or API token) | B20 | ready-for-agent |
| B23 | Accounts settings: sign in, sign out, account status, per-host token use in askpass | B20 | ready-for-agent |
| B24 | Remote repository browser: per-account lists, search, clone | B6, B23 | ready-for-agent |
| B25 | Publish a local repository: pick account, name, visibility; create, add `origin`, push | B14, B23 | ready-for-agent |

### Phase 3: purge

| # | Issue | Needs | Label |
| --- | --- | --- | --- |
| B26 | `bonsai-git`: purge engine (preflight, path scan, safety backup, fast-export filter with empty-commit pruning, fast-import, cleanup, verification, restore from backup) | B3 | ready-for-agent |
| B27 | Purge flow: "Purge from history…" on any file, history-wide path search, preview of affected commits and refs, confirmation, progress, result, force-push assistant, rotate-secret reminder, restore | B12, B14, B26, G3 | ready-for-agent |

### Phase 4: ship

| # | Issue | Needs | Label |
| --- | --- | --- | --- |
| B28 | Settings: Git binary, external editor and terminal, appearance, auto fetch | B5 | ready-for-agent |
| B29 | Performance pass: agree budgets with Arya, measure on large repositories (100k+ commits, 10k+ changes), fix regressions | B12, B7 | needs-triage |
| B30 | Release pipeline: signing, notarization and DMG, ported from `stable` | B1 | ready-for-human |

## Open questions for Arya

1. **Provider depth.** Milestone 1 stops at accounts, browsing, cloning and
   publishing. Pull requests and issues wait for milestone 2. Is that the
   right line?
2. **Minimum macOS.** Proposed macOS 14 Sonoma or later.
3. **gpuicn mode.** Proposed crate mode with ejects only by exception, so
   Bonsai tracks gpuicn releases (ADR 1). Source mode is the alternative.
4. **Self-hosted servers.** Proposed: design for base URLs now, ship GitHub
   Enterprise and self-managed GitLab later.
5. **Release.** Is milestone 1 a public signed release, or a build for you
   first?

Once these are settled and this plan is approved, the issues above get filed
with these titles, dependencies and labels.
