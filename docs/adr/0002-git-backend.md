# 2. Git CLI for operations, gix for hot reads

Status: Accepted
Date: 2026-11-15

## Context

Bonsai has to behave exactly like the user's Git: their config, hooks,
credential helpers, SSH setup, commit signing, LFS filters and `includeIf`
rules. It also has to render large histories and statuses fast.

Options considered:

- **git2 (libgit2)**: mature bindings, but a C dependency that lags Git on
  features (SSH config, partial clone, sparse checkout, some config forms) and
  ignores hooks and signing unless reimplemented.
- **gix (gitoxide)**: pure Rust, fast object and ref access, rev walks and
  status; write paths, network and hooks are incomplete or differ from Git.
- **`git` CLI**: the reference behavior by definition; costs a process spawn
  and output parsing per call. The SwiftUI app used it exclusively and it held
  up.

## Decision

A hybrid inside a GPUI-free `bonsai-git` crate:

- **All mutations and all network go through the system `git`**
  (`/usr/bin/git` or a configured path, minimum version checked at start):
  commit, checkout, merge, rebase, cherry-pick, revert, reset, stash, fetch,
  pull, push, clone, and the plumbing used by purge. Commands run with
  `GIT_TERMINAL_PROMPT=0`, a Bonsai askpass helper (ADR 3), `-c
  color.ui=never`, porcelain v2 or `-z` output where available, and are logged
  to the activity log.
- **gix handles read paths where process cost shows**: rev walks for the
  commit graph, ref listing, blob and tree reads for diffs and the file browser,
  and history scans for purge. Status starts with `git status --porcelain=v2
  -z`; switch to gix status only if measurements say so.
- Every read has a CLI fallback so a gix gap is a slowdown, not a bug.
- The crate exposes typed operations and snapshots, not command strings. Views
  never spawn processes.

## Consequences

- Behavior matches the user's terminal, including hooks and signing.
- Two code paths for some reads; tests compare gix and CLI results on
  fixture repositories.
- Integration tests need a real `git` binary, which macOS CI runners have.
