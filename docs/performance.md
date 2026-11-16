# Performance

## Budgets

Arya set these budgets on 2026-11-16 for large repositories:

| What | Budget |
|---|---|
| Scrolling and every main-thread update | 120 fps: 8 ms a frame |
| Opening a repository, to the Changes view | 250 ms |
| Showing History, to the first page | 250 ms |
| Refreshing a work tree with 10,000 changes | 150 ms |
| Memory with 100,000 commits loaded | 300 MB |

## Measuring

`crates/bonsai/tests/perf.rs` generates a repository and times Bonsai against
it. The repository has:

- 100,000 commits, with a merge from a side branch every 25 commits;
- 200 branches and 200 tags spread through the history;
- 10,000 tracked files and a commit-graph file;
- 10,000 changes in the work tree: 5,000 edited files, 5,000 new files, and
  a 50,000-line file with every line changed.

```sh
make perf
# or keep the generated repository between runs:
BONSAI_PERF_REPO=/tmp/bonsai-perf make perf
```

The test drives the real views (`RepositoryModel`, `ChangesView`,
`HistoryView`) in GPUI's test context, then times the Git work on its own. It
prints each timing, whether it is within its budget, and the process's
resident memory. The test context runs background work on the same thread,
so a page's main-thread share is the page's total less the time Git takes to
read the same page (about 10 to 18 ms of the 17 ms here, which leaves the
main thread's part within a frame).

Frame times while scrolling need a real window. The views draw only the rows
on screen (gpuicn's Virtual List, Tree and Commit graph), so a frame's cost
doesn't grow with the repository. Check them on a Mac with Instruments'
Animation Hitches template, or with `MTL_HUD_ENABLED=1`, scrolling History and
a 10,000-change Changes tree.

## Results

Run on 2026-11-16 on a 4-core Linux x86-64 cloud machine, release build, Git
2.43:

| What | Before | After | Budget |
|---|---|---|---|
| Open, to the Changes view | 184 ms | 42 ms | 250 ms |
| Show History, first page | 182 ms | 132 ms | 250 ms |
| Refresh with 10,000 changes | 62 ms | 33 ms | 150 ms |
| Slowest History page, Git's read and showing it | 234 ms | 17 ms | 8 ms for the part after Git's read |
| Page through all 104,000 commits | 35.9 s | 0.96 s | none |
| Reload after refs move, with all history loaded | 1.6 s, blocking | 0.6 s, a page at a time | none |
| Memory with all history loaded | 339 MB (see below) | 106 MB | 300 MB |

The first run measured memory after reading the whole history in one go as
well, so its 339 MB overstates the old views; the new figure is for the views
alone, measured first.

Git's own share, for reference: status of the 10,000 changes takes 40 ms, a
diff of the 50,000-line file 48 ms, and a message search across all history
0.6 s, which runs in the background.

## What changed

- **History streams from one `git log`.** `Repository::log_reader` keeps the
  process open and reads a page at a time. Each page used to be a new
  `git log --skip=N`, which walks the N skipped commits again, so the 200th
  page cost as much as reading 100,000 commits.
- **Pages are appended.** History adds each page with gpuicn 0.12.1's
  `CommitGraphState::append_rows` instead of replacing every row, and it no
  longer keeps its own copy of the rows.
- **A reload reads one page, then pages on to the selected commit.** When
  refs move (a fetch, a commit), History used to read the whole loaded
  history in one `git log`, and laid it all out on the main thread.
- **The Changes tree opens its folders at once.** gpuicn 0.12.1's
  `TreeState::expand_folders` rebuilds the visible rows once, not once per
  folder.

## Known limits

- The first History page waits on `git log --topo-order` over every branch
  and tag: 130 ms here. With a ref on an old commit, Git has to walk down to
  that commit's generation before it can order anything. A commit-graph file
  (`git commit-graph write --reachable`, which `git gc` and `git maintenance`
  write) keeps this fast. Without one it is several times slower.
- A message or author search reads the whole history in Git (0.6 s here). It
  runs in the background, and the list updates when it finishes.
