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

**Undo**:
Reversing the newest operation Bonsai ran, from the refs it changed and any
files it saved first: a commit goes back to staged changes, a switch switches
back, a deleted branch, tag or stash comes back, and a discard writes the
files back. Each undo checks the repository is still as the operation left
it, and Redo runs it again. Changes made outside Bonsai aren't undone.
_Avoid_: Revert (a new commit that reverses another)

**Repository settings**:
Values written to one repository's own config that override the global ones:
the committer name and email, whether and how commits are signed, and
whether fetching prunes deleted remote branches. Clearing one falls back to
the global value. Hooks are listed there too, and turned off by removing
their executable bit.
_Avoid_: Preferences (that's Settings, for the whole app)

**LFS**:
Git Large File Storage: files matching patterns in `.gitattributes` are
committed as a small pointer (a size and a SHA-256), and their contents live
on the LFS server. Storing a file with LFS adds its pattern; files already in
history stay as they are until purged or rewritten.
_Avoid_: Large files (any big file), annex

**New repository**:
A repository started on this Mac from File > New Repository: a folder, a
first branch, and optional starting files (a `.gitignore` for the project's
language, a README and a license), committed as "Initial commit". It has no
remote until it's published.
_Avoid_: Init (the Git command), template repository

**Content search**:
Finding text inside the tracked files, in the work tree or at a chosen
commit, from the Files view (`git grep`). A query in lowercase matches any
case; one with a capital letter matches exactly. Binary files are skipped.
_Avoid_: Find in files, filter (Files' filter matches names)

**Co-author**:
Someone else credited on a commit with a `Co-authored-by: Name <email>`
trailer at the end of its message, which GitHub and GitLab show beside the
author. Bonsai offers the people who recently committed to the repository.
_Avoid_: Pair, contributor

**Amend**:
Folding the staged changes into a commit, keeping its message. For the last
commit that is `git commit --amend`; for an earlier one on the current branch
(History > Amend with Staged Changes), a fixup commit squashed in by an
interactive rebase, which rewrites the commits after it. Undoing it leaves
the changes staged again.
_Avoid_: Fixup (Git's word for the mechanism)

**Stale branch**:
A local branch Clean Up Branches offers to delete: one merged into the main
branch (`main`, `master`, `trunk` or `develop`), or one whose upstream was
deleted on the remote ("gone"). Merged ones are ticked by default; gone ones
may hold unmerged commits, so they aren't. Deleting is local and undoable.
_Avoid_: Dead branch, old branch

**Bisect**:
A binary search through history for the first commit with a problem. The
person marks one bad and one good commit, then each commit Git checks out as
good, bad or skipped, or lets a command decide, until one commit is left.
_Avoid_: Regression finder

**Submodule**:
Another repository checked out in a folder of this one and pinned at a commit
this repository records. Updating checks out the recorded commit; updating to
latest moves it to the newest commit on its remote, a change to commit.
_Avoid_: Nested repository, dependency

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

**Pull request**:
A request on a provider to merge one branch into another; GitLab calls it a
merge request. Bonsai lists a repository's pull requests and issues from the
first remote, `origin` first, on a host with a signed-in account.
_Avoid_: PR (in UI text), merge request (except on GitLab)

**Publish**:
Creating a remote repository on a provider for a local repository that has no
remote, adding it as `origin` and pushing the current branch.
