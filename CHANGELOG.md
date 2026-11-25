# Changelog

What changed in each Bonsai release, newest first. Every release's GitHub
notes come from its section here, so a version bump must add one (see
docs/releasing.md). Write for people using Bonsai: what they can do now and
what got fixed, grouped by feature, with the pull requests in brackets.

## 0.24.0 (2026-11-25)

### Rewriting commits

- **Split Commit.** Right-click a commit in History to break it into
  smaller ones. Bonsai pauses the rebase with that commit's changes
  unstaged in Changes. Commit them in as many parts as you like, then press
  Continue. (#124)
- **Squash Merge.** Right-click a branch to bring all its changes in as
  staged changes, with no merge commit. Changes fills in a message listing
  the squashed commits, ready to edit and commit. (#123)

### Commit message

- **Prepared messages.** After a merge or squash merge that stops for
  conflicts, or one that waits for you to commit, the message box starts
  with the message Git prepared, not your template. (#123)

### History

- **Combine search filters.** Mix free text with `author:`, `path:` and the
  new `since:` and `until:`, as in `since:2w author:ada login`. Dates take
  Git's forms, like `2026-11-01` or `yesterday`, and short forms like `3d`
  and `2w`. Quote a path with spaces. (#122)
- **Compare with Work Tree.** Right-click a commit to see everything that
  differs between it and the files on disk, including changes you haven't
  committed yet. (#126)

### Files

- **Compare with Work Tree.** While browsing a branch, tag or commit,
  right-click a file to see how your copy differs from that version, for
  example what has changed in README.md since `main`. (#125)

### Stashes

- **Keep staged changes.** The new stash sheet has a "Keep staged changes"
  option. It stashes everything but leaves what you staged in place, so you
  can test or commit just that. (#121)

## 0.23.0 (2026-11-25)

### History

- **First Parent Only.** In the menu beside the search field, follow just
  each merge's first parent: a branch reads as one straight line, with
  every merged branch shown as its single merge commit. (#119)
- **Commit Staged as Fixup.** Right-click a commit to commit what you've
  staged as a `fixup!` of it, without rewriting anything yet. Fold the
  fixes in later with Edit Commits. (#117)
- **Search the history of some lines.** `lines:src/app.rs:10-20` lists the
  commits that changed those lines, following them as they move and as the
  file is renamed. (#118)

### Blame

- **History of these lines.** Each run of lines in Blame has a button that
  shows every commit that changed those lines in History. (#118)
- **Step back past a commit.** Each run of lines also has a button that
  blames the file as it was just before that commit, to see who wrote
  those lines before. Back returns. (#115)

### Edit Commits

- **Fixups line up on their own.** `fixup!` and `squash!` commits move under
  the commits they name and are set to Fixup or Squash when Edit Commits
  opens, just as `git rebase --autosquash` does. You can still change
  anything before rewriting. (#116)

## 0.22.0 (2026-11-24)

### History

- **Show one branch's history.** The menu beside the search field shows
  all branches, just the current one, or any single branch with its
  upstream. (#105)
- **Follow a file past its renames.** A file's history keeps going back
  through the commits from before it was renamed or moved. (#106)
- **Compare two commits.** Select two commits and choose Compare These
  Commits to see everything between them in Compare. (#112)
- **Revert part of a commit.** In a commit's diff, Revert Hunk and Revert
  Lines take just those changes back out of your work tree, ready to
  review. ⌘Z undoes it. (#108)

### Files

- **Restore an earlier version.** Browsing Files at a branch, tag or
  commit, right-click a file and choose Restore This Version to put that
  version in your work tree. A deleted file comes back. (#110)

### Stashes

- **New Branch from Stash.** Turns a stash into a branch that starts where
  the stash was made, so a stash that no longer applies cleanly on top of
  today's work still comes back. (#111)

### Clone

- **Recent history only.** Clone big repositories faster with only the
  latest commit of each branch. History offers Fetch Full History when you
  want the rest. (#113)
- **Include submodules** when cloning. (#113)

### GitHub, GitLab and Bitbucket

- **Open in Browser works with your own servers** too, such as GitHub
  Enterprise or a self-hosted GitLab you have signed in to. (#107)

### Fixes

- Right-clicking a section heading in the sidebar no longer opens an empty
  menu, or the last branch's menu again. (#109)

## 0.21.0 (2026-11-24)

### History

- **Select several commits.** Shift-click a range or ⌘-click commits in
  History, then cherry-pick them onto the current branch, revert them, or
  copy their hashes, each in one step. (#101)
- **Squash several commits.** With a run of commits selected, Squash N
  Commits… opens Edit Commits with them folded into the oldest, ready to
  reword. (#102)
- **Amend an earlier commit.** Stage a fix, then choose Amend with Staged
  Changes on any commit of the current branch. The changes join that
  commit and keep its message, and ⌘Z puts them back as staged. (#100)

### Branches

- **Clean Up Branches.** Repository > Clean Up Branches… lists local
  branches already merged into main and ones deleted on the remote, and
  deletes the ones you tick in one undoable step. Nothing changes on the
  remote. (#99)

### GitHub, GitLab and Bitbucket

- **Open on GitHub.** Commits in History, remote branches in the sidebar
  and files in Files have Open on GitHub (or GitLab, or Bitbucket), and
  Repository > Open in Browser opens the repository's page. (#103)

## 0.20.0 (2026-11-23)

### Commit

- **Co-authors.** A new button in the commit box lists the people who
  recently committed to the repository. Picking one adds a
  `Co-authored-by` trailer, which GitHub and GitLab show on the commit. (#93)
- **Commit templates.** When `commit.template` is set, the commit box starts
  from it and goes back to it after each commit. Commit waits until you
  have filled the template in, as `git commit` does. (#97)
- **Undo Commit keeps your message.** Undoing a commit puts its message back
  in the commit box, ready to commit again. (#92)

### Changes and stashes

- **Stash selected files.** Right-click files in Changes and choose Stash
  This File… or Stash N Files… to put only those changes aside. (#92)

### Files

- **Search inside files.** The new button next to the Files filter searches
  the text of every tracked file, in the work tree or at any branch, tag or
  commit. Results are grouped by file, and clicking a line opens the file
  at that line. (#95)

### Repositories

- **New Repository dialog.** File > New Repository (⌘N) asks for a name,
  folder and first branch, and can add a `.gitignore` for Rust, Node,
  Python, Swift, Go or Java, a README and an MIT or ISC license. Bonsai
  makes the first commit for you. (#94)

### Tags

- **Delete a tag on a remote.** A tag's menu has Delete on origin…, which
  removes it on the server and keeps the local tag. (#96)

### Fixes

- Pushing a tag now asks for credentials when the remote needs them, like
  branch pushes do, instead of failing. (#96)
- The Clone and New Repository dialogs put the cursor in their first field.
  (#94)
- A file's name no longer overlaps its size in the Files viewer when the
  pane is narrow. (#95)

## 0.19.0 (2026-11-23)

### Undo and redo

- **Undo almost anything.** ⌘Z undoes the last commit, branch switch,
  discard, reset, stash, branch deletion or rename, and ⇧⌘Z redoes it, also
  from the new Edit menu. Each finished operation's toast has an Undo
  button. Discarded files are backed up first, so a discard can be undone
  too. (#88)

### Repository settings

- **Repository Settings (⌥⌘,).** Set this repository's own name and email,
  commit signing (off, OpenPGP or SSH) and whether fetching prunes deleted
  branches, all overriding your global Git settings. Hooks are listed and
  can be turned on and off. (#89)

### Git LFS

- **Large files with Git LFS.** LFS files show as a card with their size and
  hash instead of a pointer diff. Store a file, or every file of its type,
  with LFS from Changes, and see the stored patterns and download LFS files
  in Repository Settings. (#90)

### Patches

- **Patches.** Copy a commit as a patch or save it to a file from History,
  and apply a patch file with Repository > Apply Patch…. (#91)

## 0.18.0 (2026-11-23)

### History

- **Find where a line of code came from.** Search History with `code:` to
  find the commits that added or removed some text, or choose Find Where
  This Changed from a diff. (#86)
- **Bisect.** History > Find a Bad Commit walks you through `git bisect`
  with a bar that marks each commit good, bad or skipped, until the commit
  that broke something is found. (#84)

### Purge from History

- **Find secrets and large files.** Purge from History suggests files that
  look like secrets (such as `.env` and keys) or are unusually large, across
  all of history, so you can purge them in one go. (#83)

### Submodules

- **Submodules.** A Submodules section in the sidebar lists them with their
  state. Add, clone, update to the latest from their remote, open in a tab
  or remove them, and see a submodule's commit range in diffs. (#85)

## 0.17.0 (2026-11-22)

### Compare

- **Compare two branches (⌘8).** Pick a base and a branch to see the
  commits only on each side and one diff of everything that changed. (#80)

### Reflog

- **Find lost commits.** The Reflog section lists everywhere HEAD has been,
  including commits no branch points at any more, and restores one as a new
  branch. (#79)

### Conflicts

- **Resolve conflicts region by region.** A conflicted file shows each
  conflict with Use Current, Use Incoming and Use Both buttons. (#81)

### Pull requests and issues

- **Edit titles and labels** of pull requests and issues from Bonsai. (#78)

### App

- **A new logo and app icon.** (#77)

## 0.16.0 (2026-11-17)

### Files

- **Stop Tracking and Ignore.** Right-click a file in Files to stop tracking
  it and add it to `.gitignore` in one step. (#76)

## 0.15.0 (2026-11-16)

### Files

- **Browse files at any branch, tag or commit.** The menu at the top of
  Files switches between the work tree and any revision, and History has
  Browse Files for a commit. (#75)

## 0.14.0 (2026-11-16)

### Files

- **Syntax colors** in the Files viewer, for common languages. (#74)

## 0.13.0 (2026-11-16)

### Files

- **A Files section (⌘6).** Browse every tracked file as a folder tree and
  read it, with Blame, its History, Purge from History, Copy Path and
  Reveal in Finder. (#73)

## 0.12.0 (2026-11-16)

### Pull requests

- Bitbucket pull requests show their changed lines, not only the file
  names. (#72)
- Fixes requests to providers that answer with a redirect. (#72)

## 0.11.0 (2026-11-16)

0.11.0 also carries 0.10.0, which was never published.

### Pull requests

- **Reply to line comment threads.** (#70)
- **Resolve and reopen threads** on GitHub and GitLab. (#71)

## 0.9.0 (2026-11-16)

### Pull requests

- **Comment on a line.** Click a line in a pull request's files to leave a
  comment on it. (#69)

## 0.8.0 (2026-11-16)

### Pull requests

- **Line comments** show under the lines they are about in a pull
  request's files. (#68)

## 0.7.0 (2026-11-16)

### Pull requests

- **Changed files.** A pull request has Conversation and Files tabs, and
  Files shows its diff. (#66)
- **Review.** Approve a pull request or request changes. GitLab supports
  approving only. (#67)

## 0.6.0 (2026-11-16)

### Pull requests and issues

- **Comment, close and reopen** pull requests and issues. (#64)
- **CI checks** show on each pull request. (#65)

## 0.5.0 (2026-11-16)

### Pull requests and issues

- **Read the conversation** on a pull request or issue. (#59)
- **Merge pull requests** from Bonsai, with merge, squash or rebase. (#59)
- **New Issue.** Open an issue without leaving Bonsai. (#59)

## 0.4.0 (2026-11-16)

### Pull requests and issues

- **Pull Requests and Issues** sections for GitHub, GitLab and Bitbucket:
  list them and check a pull request's branch out. (#56)
- **New Pull Request** from the current branch. (#57)

### History and branches

- **Blame** shows who last changed each line of a file. (#53)
- **Worktrees.** Check branches out side by side in their own folders. (#54)
- **Edit Commits.** Reorder, reword, squash and drop commits with an
  interactive rebase you drive from a list. (#55)

## 0.3.0 (2026-11-16)

### Design

- **A new look.** A real title bar with a branch switcher, toasts for
  finished operations, a reworked Changes list and commit box, a diff pane
  with hunk buttons and image previews, History with avatars and a commit
  card, and redesigned Activity, Welcome and sheets. (#46, #47, #48, #49,
  #50, #51)

## 0.2.0 (2026-11-16)

The first release of Bonsai rebuilt in Rust and GPUI. (#14, #45)

- Open, create and clone repositories, in tabs.
- Stage, unstage and discard files or single lines, and commit with amend,
  signing and recent messages.
- A diff pane, and History with the commit graph, fast on large
  repositories.
- Branches, remote branches and tags; fetch, pull and push; merge, rebase,
  cherry-pick, revert, reset and conflict handling; stashes.
- Purge a file, such as `.env`, from all of history, with a backup and a
  force push assistant.
- Sign in to GitHub, GitLab and Bitbucket, browse and clone your remote
  repositories, and publish a local repository.
- An Activity log of every Git command, credentials from the Keychain,
  Settings and a command palette.
