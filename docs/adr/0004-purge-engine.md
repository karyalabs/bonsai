# 4. Purge through git fast-export and fast-import

Status: Proposed
Date: 2026-11-15

## Context

Purging a committed secret such as `.env` from all history is a signature
feature. The SwiftUI app used `git filter-branch`, which Git itself deprecates
as slow and error-prone. `git filter-repo` is the recommended tool but is a
Python script that macOS does not ship.

## Decision

Bonsai implements purge natively as a stream filter:

1. Preflight: clean work tree, no in-progress operation, no other worktree mid
   operation. Scan all local refs (gix) and report which commits, branches and
   tags contain the paths.
2. Write a safety backup (`git bundle create … --all`) to Bonsai's application
   support folder.
3. Run `git fast-export --all --no-data --signed-tags=strip
   --tag-of-filtered-object=rewrite --reencode=yes`, filter the stream in Rust
   to drop `M`/`D` entries for the purged paths (exact paths, directories and
   globs), prune commits that become empty (but keep merges and commits that
   were empty to begin with) and remap their marks, then feed it to `git
   fast-import --force`.
4. Reset the index to the new HEAD, expire reflogs, drop the stash if it
   referenced the path (after warning), and `git gc --prune=now`.
5. Verify no reachable object has the path, and report counts.

Bonsai never force-pushes as part of a purge. Afterwards it lists remote
branches that now diverge and offers an explicit "Force push with lease" per
remote, and tells the user to rotate the secret, since remote caches, forks and
pull request refs may keep the old commits.

A prototype of steps 3–4 on a test repository removed `.env` from every
commit and object while the file stayed on disk.

## Consequences

- No Python or extra binary to bundle; works with the system `git`.
- Bonsai owns a stream parser for the fast-export format, which needs thorough
  fixture tests (merges, tags, renames off, non-UTF-8 paths, submodules).
- Signed commits lose their signatures, and commit hashes change; the UI must
  say so before the user confirms.
