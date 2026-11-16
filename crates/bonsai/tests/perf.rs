//! Measures Bonsai on a large repository: 100,000 commits with merges, 10,000
//! tracked files, 10,000 changes in the work tree and a 50,000-line diff.
//!
//! Ignored by default because it takes a few minutes. Run it with
//!
//! ```sh
//! cargo test -p bonsai --release --test perf -- --ignored --nocapture
//! ```
//!
//! Set `BONSAI_PERF_REPO` to a folder to keep the generated repository
//! between runs. Results and budgets are recorded in `docs/performance.md`.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use bonsai::{
    changes::{ChangesView, group_rows, tree_nodes},
    history::HistoryView,
    refs::RefsPanel,
    repository::RepositoryModel,
};
use bonsai_git::{DiffOptions, DiffTarget, Git, LogFilter, LogScope, Repository};
use bonsai_graph::{Commit, Layout};
use gpui_kit::{AppContext as _, TestAppContext};

const COMMITS: usize = 100_000;
const FILES: usize = 10_000;
const BIG_LINES: usize = 50_000;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn file(n: usize) -> String {
    format!("src/d{:03}/file{n:05}.txt", n % 100)
}

/// Writes the history with one `git fast-import` stream: a first commit with
/// every file, then commits that each change one file, a merge from a side
/// branch every 25 commits, and a branch and a tag every 500.
fn generate(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    git(root, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.name", "Bonsai"),
        ("user.email", "bonsai@example.com"),
        ("commit.gpgsign", "false"),
    ] {
        git(root, &["config", key, value]);
    }
    let mut stream = String::new();
    let mut time = 1_600_000_000_u64;
    let mut commit = |stream: &mut String, r#ref: &str, mark: usize, message: &str| {
        time += 60;
        let _ = write!(
            stream,
            "commit {}\nmark :{mark}\ncommitter Ada <ada@example.com> {time} +0000\ndata {}\n{message}\n",
            r#ref,
            message.len()
        );
    };
    let inline = |stream: &mut String, path: &str, data: &str| {
        let _ = write!(
            stream,
            "M 100644 inline {path}\ndata {}\n{data}\n",
            data.len()
        );
    };

    commit(&mut stream, "refs/heads/main", 1, "Initial commit");
    for n in 0..FILES {
        inline(&mut stream, &file(n), &format!("file {n}\n"));
    }
    let big: String = (0..BIG_LINES).fold(String::new(), |mut big, n| {
        let _ = writeln!(big, "line {n}");
        big
    });
    inline(&mut stream, "big.txt", &big);
    stream.push('\n');

    let mut main = 1;
    let mut mark = 1;
    for n in 1..COMMITS {
        mark += 1;
        if n % 25 == 0 {
            // A side commit off the last main commit, then a merge of it.
            commit(
                &mut stream,
                "refs/heads/side",
                mark,
                &format!("Side work {n}"),
            );
            let _ = writeln!(stream, "from :{main}");
            inline(&mut stream, &format!("side/{n}.txt"), "side\n");
            stream.push('\n');
            let side = mark;
            mark += 1;
            commit(
                &mut stream,
                "refs/heads/main",
                mark,
                &format!("Merge side work {n}"),
            );
            let _ = writeln!(stream, "from :{main}\nmerge :{side}");
        } else {
            commit(
                &mut stream,
                "refs/heads/main",
                mark,
                &format!("Change file {} (commit {n})", n % FILES),
            );
            let _ = writeln!(stream, "from :{main}");
            inline(
                &mut stream,
                &file(n % FILES),
                &format!("file {} at {n}\n", n % FILES),
            );
        }
        stream.push('\n');
        main = mark;
        if n % 500 == 0 {
            let _ = write!(
                stream,
                "reset refs/heads/feature/{n}\nfrom :{main}\n\nreset refs/tags/v{n}\nfrom :{main}\n\n"
            );
        }
    }
    let mut child = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), stream.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    git(root, &["checkout", "-qf", "main"]);
    git(root, &["commit-graph", "write", "--reachable"]);

    // 10,000 changes: half the tracked files edited, 5,000 new files, and
    // every line of the big file changed.
    for n in (0..FILES).step_by(2) {
        std::fs::write(root.join(file(n)), format!("edited {n}\n")).unwrap();
    }
    for n in 0..5_000 {
        let path = root.join(format!("new/d{:02}/new{n:04}.txt", n % 50));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "new\n").unwrap();
    }
    let edited: String = (0..BIG_LINES).fold(String::new(), |mut big, n| {
        let _ = writeln!(big, "changed line {n}");
        big
    });
    std::fs::write(root.join("big.txt"), edited).unwrap();
    std::fs::write(root.join(".generated"), "").unwrap();
}

fn repository() -> (Option<tempfile::TempDir>, PathBuf) {
    let (temp, root) = if let Some(path) = std::env::var_os("BONSAI_PERF_REPO") {
        (None, PathBuf::from(path))
    } else {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("large");
        (Some(temp), root)
    };
    if !root.join(".generated").exists() {
        let started = Instant::now();
        generate(&root);
        println!("generated the repository in {:.1?}", started.elapsed());
    }
    (temp, root)
}

/// Resident memory of this process, in megabytes (Linux and macOS).
fn resident_mb() -> f64 {
    let pid = std::process::id().to_string();
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0)
        / 1024.0
}

struct Report(Vec<(String, Duration, Option<Duration>)>);

impl Report {
    fn time<T>(&mut self, name: &str, budget: Option<u64>, work: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let out = work();
        self.record(name, started.elapsed(), budget);
        out
    }

    fn record(&mut self, name: &str, took: Duration, budget: Option<u64>) {
        let budget = budget.map(Duration::from_millis);
        let verdict = match budget {
            Some(b) if took > b => format!("  MISSES {b:?}"),
            Some(b) => format!("  within {b:?}"),
            None => String::new(),
        };
        println!("{name:<52} {took:>10.1?}{verdict}");
        self.0.push((name.to_owned(), took, budget));
    }
}

/// Arya's budgets (docs/performance.md): 120 fps (8 ms frames), open in
/// 250 ms, refresh 10k changes in 150 ms, under 300 MB with 100k commits.
const FRAME_MS: u64 = 8;
const OPEN_MS: u64 = 250;
const REFRESH_MS: u64 = 150;
const MEMORY_MB: f64 = 300.0;

#[gpui_kit::test]
#[ignore = "takes minutes; run with --ignored --release"]
fn large_repository(cx: &mut TestAppContext) {
    let (_temp, root) = repository();
    let git_binary = Git::locate(None).unwrap();
    let repository = Repository::open(&git_binary, &root).unwrap();
    let mut report = Report(Vec::new());
    cx.update(bonsai::ui::theme::init);
    let memory_before = resident_mb();

    // The app: open the repository, refresh, page through all of history.
    let started = Instant::now();
    let model = cx.new(|cx| RepositoryModel::new(repository.clone(), false, cx));
    cx.run_until_parked();
    report.record("open: first snapshot", started.elapsed(), None);
    let step = Instant::now();
    let changes = cx.new(|cx| ChangesView::new(model.clone(), cx));
    cx.run_until_parked();
    report.record("open: changes view", step.elapsed(), None);
    report.record("open in total", started.elapsed(), Some(OPEN_MS));
    let step = Instant::now();
    let refs = cx.new(|cx| RefsPanel::new(model.clone(), cx));
    let history = cx.new(|cx| HistoryView::new(model.clone(), refs, cx));
    history.update(cx, HistoryView::start);
    cx.run_until_parked();
    report.record("show History: first page", step.elapsed(), Some(OPEN_MS));
    changes.read_with(cx, |view, _| assert!(view.rows().len() > 10_000));

    let started = Instant::now();
    model.update(cx, RepositoryModel::refresh);
    cx.run_until_parked();
    report.record(
        "refresh with 10k changes",
        started.elapsed(),
        Some(REFRESH_MS),
    );

    let started = Instant::now();
    let mut slowest = Duration::ZERO;
    let mut pages = 0;
    while !history.read_with(cx, |view, _| view.is_exhausted()) {
        let page = Instant::now();
        history.update(cx, HistoryView::load_more);
        cx.run_until_parked();
        slowest = slowest.max(page.elapsed());
        pages += 1;
    }
    let loaded = history.read_with(cx, |view, _| view.commits().len());
    report.record(
        &format!("page through all {loaded} commits ({pages} pages)"),
        started.elapsed(),
        None,
    );
    report.record("slowest page, read and shown", slowest, None);

    // How much of a page is Git, read on a background thread: the rest is
    // the window's own work, which must fit in a frame.
    let mut reader = repository
        .log_reader(&LogScope::All, &LogFilter::default())
        .unwrap();
    let mut slowest_read = Duration::ZERO;
    let mut first = true;
    while !reader.is_done() {
        let read = Instant::now();
        reader.next_page(500).unwrap();
        if !first {
            slowest_read = slowest_read.max(read.elapsed());
        }
        first = false;
    }
    report.record("slowest later page read by Git", slowest_read, None);
    report.record(
        "slowest page's work on the main thread (about)",
        slowest.saturating_sub(slowest_read),
        Some(FRAME_MS),
    );

    let started = Instant::now();
    history.update(cx, |view, cx| {
        let deep = view.commits()[loaded / 2].oid.clone();
        view.reveal(&deep, cx);
    });
    cx.run_until_parked();
    report.record("show a commit 50,000 deep", started.elapsed(), None);
    let started = Instant::now();
    history.update(cx, HistoryView::reload);
    cx.run_until_parked();
    report.record(
        "reload after refs move, keeping it selected",
        started.elapsed(),
        None,
    );
    history.read_with(cx, |view, _| assert!(view.selected().is_some()));

    let memory = resident_mb();
    println!(
        "{:<52} {memory:>8.0} MB (was {memory_before:.0} MB){}",
        "memory with all history loaded",
        if memory > MEMORY_MB {
            "  MISSES"
        } else {
            "  within"
        }
    );

    // The Git side, on its own.
    let snapshot = report.time("snapshot (status of 10k changes)", None, || {
        repository.snapshot().unwrap()
    });
    let rows = report.time("group the changes into rows", None, || {
        group_rows(&snapshot.changes)
    });
    report.time("build the changes tree", None, || tree_nodes(&rows));
    report.time("first history page (500)", None, || {
        repository.log(&LogScope::All, 0, 500).unwrap()
    });
    let all = report.time("whole history in one read", None, || {
        repository.log(&LogScope::All, 0, COMMITS * 2).unwrap()
    });
    report.time("lay out the whole graph", None, || {
        let commits: Vec<Commit<_>> = all
            .iter()
            .map(|c| Commit {
                id: c.oid.clone(),
                parents: c.parents.clone(),
            })
            .collect();
        Layout::new().extend(&commits).len()
    });
    report.time("search messages across history", None, || {
        let filter = LogFilter {
            message: Some("commit 99999".to_owned()),
            ..LogFilter::default()
        };
        repository
            .log_filtered(&LogScope::All, &filter, 0, 500)
            .unwrap()
    });
    report.time("diff of the 50,000-line file", None, || {
        repository
            .diff(
                &DiffTarget::Unstaged(vec!["big.txt".into()]),
                DiffOptions::default(),
            )
            .unwrap()
    });

    let misses: Vec<_> = report
        .0
        .iter()
        .filter(|(_, took, budget)| budget.is_some_and(|b| *took > b))
        .map(|(name, _, _)| name.as_str())
        .collect();
    println!("misses: {misses:?}");
}
