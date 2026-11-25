use std::{fmt::Write as _, path::Path, time::Instant};

use bonsai_git::{Git, LineRange, LogFilter, LogScope, Repository, SignatureStatus};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn run(path: &Path, args: &[&str]) -> String {
    let mut full = vec![
        "-c",
        "user.name=Ada",
        "-c",
        "user.email=ada@example.com",
        "-c",
        "commit.gpgsign=false",
    ];
    full.extend_from_slice(args);
    git()
        .run(&git().command(full).cwd(path))
        .unwrap()
        .stdout_lossy()
}

#[test]
fn pages_cover_the_history_in_topological_order() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    assert_eq!(repository.log(&LogScope::All, 0, 10).unwrap(), []);
    assert_eq!(repository.log(&LogScope::Head, 0, 10).unwrap(), []);

    run(dir.path(), &["commit", "--allow-empty", "-qm", "base"]);
    run(dir.path(), &["switch", "-qc", "feature"]);
    run(
        dir.path(),
        &["commit", "--allow-empty", "-qm", "feature work"],
    );
    run(dir.path(), &["switch", "-q", "main"]);
    run(dir.path(), &["commit", "--allow-empty", "-qm", "main work"]);
    run(
        dir.path(),
        &["merge", "--no-ff", "-qm", "Merge feature", "feature"],
    );

    let all = repository.log(&LogScope::All, 0, 100).unwrap();
    let summaries: Vec<&str> = all.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries.len(), 4);
    assert_eq!(summaries[0], "Merge feature");
    assert_eq!(summaries[3], "base");
    assert_eq!(all[0].parents.len(), 2);
    assert_eq!(all[0].author_name, "Ada");

    let first_parent: Vec<String> = repository
        .log_filtered(
            &LogScope::Head,
            &LogFilter {
                first_parent: true,
                ..LogFilter::default()
            },
            0,
            100,
        )
        .unwrap()
        .into_iter()
        .map(|c| c.summary)
        .collect();
    assert_eq!(first_parent, ["Merge feature", "main work", "base"]);

    let first = repository.log(&LogScope::All, 0, 2).unwrap();
    let second = repository.log(&LogScope::All, 2, 2).unwrap();
    assert_eq!([first, second].concat(), all);

    let feature = repository
        .log(&LogScope::Refs(vec!["feature".into()]), 0, 100)
        .unwrap();
    assert_eq!(feature.len(), 2);
}

/// `cargo test -p bonsai-git --test log -- --ignored` times a 100k-commit walk.
#[test]
#[ignore = "benchmark: builds a 100,000-commit repository"]
fn walks_one_hundred_thousand_commits() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    // fast-import builds the history in a second or two.
    let mut input = String::new();
    for i in 0..100_000_u64 {
        let message = format!("commit {i}");
        let _ = write!(
            input,
            "commit refs/heads/main\nmark :{}\ncommitter Ada <ada@example.com> {} +0000\ndata {}\n{message}\n",
            i + 1,
            1_600_000_000 + i,
            message.len()
        );
        if i > 0 {
            let _ = writeln!(input, "from :{i}");
        }
        input.push('\n');
    }
    git()
        .run(
            &git()
                .command(["fast-import", "--quiet"])
                .cwd(dir.path())
                .stdin(input),
        )
        .unwrap();

    let started = Instant::now();
    let page = repository.log(&LogScope::All, 0, 500).unwrap();
    let first_page = started.elapsed();
    let started = Instant::now();
    let all = repository.log(&LogScope::All, 0, 100_000).unwrap();
    let whole = started.elapsed();
    assert_eq!(page.len(), 500);
    assert_eq!(all.len(), 100_000);
    eprintln!("first page of 500: {first_page:?}; all 100,000: {whole:?}");
    assert!(first_page.as_secs() < 5);
}

#[test]
fn filters_by_message_author_and_path() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    std::fs::create_dir(dir.path().join("docs")).unwrap();
    std::fs::write(dir.path().join("docs/guide.md"), "g").unwrap();
    run(dir.path(), &["add", "."]);
    run(dir.path(), &["commit", "-qm", "Write the guide"]);
    std::fs::write(dir.path().join("app.rs"), "a").unwrap();
    run(dir.path(), &["add", "."]);
    run(
        dir.path(),
        &[
            "-c",
            "user.name=Grace",
            "commit",
            "-qm",
            "Fix the [app] crash",
        ],
    );
    let summaries = |filter: LogFilter| -> Vec<String> {
        repository
            .log_filtered(&LogScope::All, &filter, 0, 100)
            .unwrap()
            .into_iter()
            .map(|c| c.summary)
            .collect()
    };
    // Fixed strings, so brackets are not a pattern; case is ignored.
    assert_eq!(
        summaries(LogFilter {
            message: Some("[APP]".into()),
            ..LogFilter::default()
        }),
        ["Fix the [app] crash"]
    );
    assert_eq!(
        summaries(LogFilter {
            author: Some("grace".into()),
            ..LogFilter::default()
        }),
        ["Fix the [app] crash"]
    );
    assert_eq!(
        summaries(LogFilter {
            path: Some("docs".into()),
            ..LogFilter::default()
        }),
        ["Write the guide"]
    );
    // Both must match.
    assert_eq!(
        summaries(LogFilter {
            message: Some("guide".into()),
            author: Some("grace".into()),
            ..LogFilter::default()
        }),
        Vec::<String>::new()
    );
}

#[test]
fn finds_commits_that_add_or_remove_code() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repository = Repository::init(&git(), root, Some("main")).unwrap();
    for (contents, message) in [
        ("fn main() {}\n", "Start"),
        ("fn main() { retry(3) }\n", "Retry three times"),
        ("fn main() { retry(3) }\n// notes\n", "Add notes"),
        ("fn main() { run() }\n// notes\n", "Stop retrying"),
    ] {
        std::fs::write(root.join("main.rs"), contents).unwrap();
        run(root, &["add", "."]);
        run(root, &["commit", "-qm", message]);
    }
    let summaries: Vec<String> = repository
        .log_filtered(
            &LogScope::All,
            &LogFilter {
                code: Some("retry(3)".into()),
                ..LogFilter::default()
            },
            0,
            100,
        )
        .unwrap()
        .into_iter()
        .map(|c| c.summary)
        .collect();
    // Literal text, not a pattern; the commit that only moved lines around
    // it doesn't count.
    assert_eq!(summaries, ["Stop retrying", "Retry three times"]);
}

#[test]
fn follows_a_range_of_lines_back_through_renames() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repository = Repository::init(&git(), root, Some("main")).unwrap();
    for (file, contents, message) in [
        ("a.txt", "one\ntwo\nthree\n", "Start"),
        ("a.txt", "one\nTWO\nthree\n", "Shout two"),
        ("a.txt", "zero\none\nTWO\nthree\n", "Add zero"),
        ("b.txt", "zero\none\nTWO\nthree\n", "Rename"),
        ("b.txt", "zero\none\nTWO\nTHREE\n", "Shout three"),
    ] {
        if file == "b.txt" && !root.join("b.txt").exists() {
            run(root, &["mv", "a.txt", "b.txt"]);
        }
        std::fs::write(root.join(file), contents).unwrap();
        run(root, &["add", "."]);
        run(root, &["commit", "-qm", message]);
    }
    let history = |start, end, revision: Option<&str>| -> Vec<String> {
        let filter = LogFilter {
            lines: Some(LineRange {
                path: "b.txt".into(),
                start,
                end,
                revision: revision.map(str::to_owned),
            }),
            ..LogFilter::default()
        };
        let mut reader = repository.log_reader(&LogScope::All, &filter).unwrap();
        reader
            .next_page(100)
            .unwrap()
            .into_iter()
            .map(|c| c.summary)
            .collect()
    };
    // Line 3, "TWO": unchanged by the shift down or the rename.
    assert_eq!(history(3, 3, None), ["Shout two", "Start"]);
    assert_eq!(history(3, 4, None), ["Shout three", "Shout two", "Start"]);
    assert_eq!(history(3, 3, Some("HEAD~1")), ["Shout two", "Start"]);
}

#[test]
fn narrows_by_commit_date() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repository = Repository::init(&git(), root, Some("main")).unwrap();
    for (date, message) in [
        ("2026-01-10T12:00:00Z", "January"),
        ("2026-02-10T12:00:00Z", "February"),
        ("2026-03-10T12:00:00Z", "March"),
    ] {
        let g = git();
        g.run(
            &g.command([
                "-c",
                "user.name=Ada",
                "-c",
                "user.email=ada@example.com",
                "commit",
                "--allow-empty",
                "-qm",
                message,
            ])
            .cwd(root)
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date),
        )
        .unwrap();
    }
    let summaries = |since: Option<&str>, until: Option<&str>| -> Vec<String> {
        let filter = LogFilter {
            since: since.map(str::to_owned),
            until: until.map(str::to_owned),
            ..LogFilter::default()
        };
        repository
            .log_filtered(&LogScope::All, &filter, 0, 100)
            .unwrap()
            .into_iter()
            .map(|c| c.summary)
            .collect()
    };
    assert_eq!(summaries(Some("2026-02-01"), None), ["March", "February"]);
    assert_eq!(summaries(None, Some("2026-02-01")), ["January"]);
    assert_eq!(
        summaries(Some("2026-02-01"), Some("2026-03-01")),
        ["February"]
    );
}

#[test]
fn reads_a_commits_details() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    run(dir.path(), &["commit", "--allow-empty", "-qm", "base"]);
    run(
        dir.path(),
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "Summary line",
            "-m",
            "Body with | bars\nand lines.",
        ],
    );
    let head = repository.log(&LogScope::Head, 0, 1).unwrap().remove(0);
    let details = repository.commit_details(&head.oid).unwrap();
    assert_eq!(details.oid, head.oid);
    assert_eq!(details.parents.len(), 1);
    assert_eq!(details.author_name, "Ada");
    assert_eq!(details.committer_email, "ada@example.com");
    assert!(
        details
            .message
            .starts_with("Summary line\n\nBody with | bars")
    );
    assert_eq!(details.signature, SignatureStatus::Unsigned);
}

#[test]
fn a_reader_pages_through_the_same_history_as_log() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    for n in 0..7 {
        std::fs::write(dir.path().join("file.txt"), n.to_string()).unwrap();
        run(dir.path(), &["add", "."]);
        run(dir.path(), &["commit", "-qm", &format!("commit {n}")]);
    }
    let all = repository.log(&LogScope::All, 0, 100).unwrap();
    let mut reader = repository
        .log_reader(&LogScope::All, &LogFilter::default())
        .unwrap();
    let mut paged = Vec::new();
    let mut sizes = Vec::new();
    while !reader.is_done() {
        let page = reader.next_page(3).unwrap();
        sizes.push(page.len());
        paged.extend(page);
    }
    assert_eq!(sizes, [3, 3, 1]);
    assert_eq!(paged, all);
    assert_eq!(reader.next_page(3).unwrap(), Vec::new());

    // Dropping a reader part way stops its `git log`.
    let mut partial = repository
        .log_reader(&LogScope::All, &LogFilter::default())
        .unwrap();
    assert_eq!(partial.next_page(2).unwrap().len(), 2);
    drop(partial);
}

#[test]
fn a_reader_of_an_unborn_branch_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    let mut reader = repository
        .log_reader(&LogScope::Head, &LogFilter::default())
        .unwrap();
    assert_eq!(reader.next_page(10).unwrap(), Vec::new());
    assert!(reader.is_done());
}

#[test]
fn a_file_history_follows_renames() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repository = Repository::init(&git(), root, Some("main")).unwrap();
    std::fs::write(
        root.join("old.rs"),
        "fn main() {}\n// a long enough file to be seen as renamed\n",
    )
    .unwrap();
    run(root, &["add", "."]);
    run(root, &["commit", "-qm", "Start"]);
    run(root, &["mv", "old.rs", "new.rs"]);
    run(root, &["commit", "-qm", "Rename"]);
    std::fs::write(
        root.join("new.rs"),
        "fn main() {}\n// a long enough file to be seen as renamed\n// more\n",
    )
    .unwrap();
    run(root, &["commit", "-qam", "Edit"]);
    let summaries = |path: &str| -> Vec<String> {
        repository
            .log_filtered(
                &LogScope::All,
                &LogFilter {
                    path: Some(path.into()),
                    ..LogFilter::default()
                },
                0,
                100,
            )
            .unwrap()
            .into_iter()
            .map(|c| c.summary)
            .collect()
    };
    assert_eq!(summaries("new.rs"), ["Edit", "Rename", "Start"]);
    // A deleted file's history still reads.
    assert_eq!(summaries("old.rs"), ["Rename", "Start"]);
}
