# Bonsai

A native macOS Git client built with Rust, GPUI and gpuicn. Bonsai aims to make
every Git operation, from a first commit to a history rewrite, safe and visible
without a terminal.

## Language

**Repository**:
A local Git repository Bonsai has opened, identified by its canonical work tree
path (or git dir for a bare repository). A repository may have no remotes.
_Avoid_: Project, workspace, repo (in code identifiers)

**Snapshot**:
An immutable, point-in-time read of a repository's state: HEAD, refs, status,
in-progress operation and configuration Bonsai cares about. Views render
snapshots; they never query Git directly.
_Avoid_: State, model

**Change**:
One path in the working tree or index that differs from HEAD, with its index
and work tree status. Changes are grouped as staged, unstaged, untracked and
conflicted.
_Avoid_: File status, modification

**Hunk**:
A contiguous block of a diff with its header range. Hunks and individual lines
are the units of partial staging and discarding.

**Operation**:
A mutation Bonsai runs against a repository (commit, checkout, merge, push,
purge…). Operations run one at a time per repository through the operation
queue, record their commands in the activity log and end with a fresh snapshot.
_Avoid_: Action (reserve for GPUI actions), command (reserve for a Git process)

**Activity log**:
The per-repository record of operations and the exact Git commands they ran,
with output, duration and result.

**Purge**:
Removing one or more paths from every commit reachable from local refs, then
expiring reflogs and pruning so the objects no longer exist locally. Purging
never pushes by itself.
_Avoid_: Remove from history, scrub, filter

**Safety backup**:
A Git bundle of all refs written before a purge or other history rewrite, so
the rewrite can be undone.

**Provider**:
A Git hosting service Bonsai speaks to over its API: GitHub, GitLab or
Bitbucket. A remote on any other server is a plain remote with no provider.
_Avoid_: Host, integration, forge

**Account**:
One signed-in identity on one provider host. Tokens live in the macOS Keychain.

**Remote repository**:
A repository listed by a provider account. It may or may not be cloned
locally.
_Avoid_: Cloud repo, online repo

**Publish**:
Creating a remote repository on a provider for a local repository that has no
remote, adding it as `origin` and pushing the current branch.
