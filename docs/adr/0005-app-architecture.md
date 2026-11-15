# 5. App structure: snapshot entities, an operation queue, and a watcher

Status: Accepted
Date: 2026-11-15

## Context

The SwiftUI app concentrated state in one 2,900-line `RepositoryStore`. The
rebuild needs clear seams so features can be added in parallel issues.

## Decision

Workspace layout:

```text
crates/
  bonsai-git/        typed Git reads and operations, snapshots, purge (no GPUI)
  bonsai-graph/      commit graph lane layout (no GPUI)
  bonsai-providers/  provider trait and GitHub, GitLab, Bitbucket (no GPUI)
  bonsai-keychain/   macOS Keychain access and the askpass entry point
  bonsai/            the GPUI app: windows, views, actions, settings
```

In the app:

- One GPUI `Entity<RepositoryModel>` per open repository holds the latest
  `Snapshot`, cached history pages and diffs, and the activity log. Views
  observe it and keep only view state (selection, scroll, pane sizes).
- Mutations go through a per-repository **operation queue** on the background
  executor, one at a time, each ending with a fresh snapshot. Reads run
  concurrently and are cancelled when their result is stale.
- An FSEvents watcher (`notify`) on the work tree and git dir debounces into
  refreshes, skipping Bonsai's own operations while they run.
- User-facing commands are GPUI actions, bound to keys, the native menu bar
  and the command palette from one table, so each appears everywhere.
- Destructive operations share one confirmation pattern (AlertDialog) and one
  undo path where Git allows it.
- Settings and recents persist as JSON in Application Support; per-window
  layout restores on launch.

## Consequences

- `bonsai-git`, `bonsai-graph` and `bonsai-providers` are tested without a
  window; the app crate stays thin.
- Every feature issue names which crate it touches, which keeps issues
  parallel.
