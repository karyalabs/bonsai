use std::time::{Duration, SystemTime};

use bonsai_git::CommandRecord;

fn record(args: &[&str]) -> CommandRecord {
    CommandRecord {
        args: args.iter().map(|&a| a.to_owned()).collect(),
        cwd: None,
        started_at: SystemTime::now(),
        duration: Duration::from_millis(5),
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        cancelled: false,
    }
}

#[test]
fn command_lines_paste_into_a_shell() {
    assert_eq!(
        record(&["commit", "-m", "it's done", "--", "a b.txt"]).command_line(),
        r"git commit -m 'it'\''s done' -- 'a b.txt'"
    );
    assert_eq!(
        record(&["push", "origin", "HEAD:main"]).command_line(),
        "git push origin HEAD:main"
    );
    assert_eq!(record(&["log", ""]).command_line(), "git log ''");
}

#[test]
fn reads_are_told_apart_from_changes() {
    for read in [
        &["status", "--porcelain=v2"][..],
        &["for-each-ref", "--format=%(refname)"],
        &["stash", "list"],
        &["config", "--get", "user.name"],
        &["branch", "--list"],
        &["remote"],
        &["reflog", "-1", "--format=%gs"],
    ] {
        assert!(record(read).is_read_only(), "{read:?}");
    }
    for change in [
        &["commit", "-m", "x"][..],
        &["stash", "push"],
        &["config", "user.name", "Ada"],
        &["branch", "-d", "topic"],
        &["remote", "add", "origin", "url"],
        &["push", "origin", "main"],
        &["fetch", "--prune"],
        &["reflog", "expire", "--all"],
    ] {
        assert!(!record(change).is_read_only(), "{change:?}");
    }
    let mut failed = record(&["status"]);
    failed.exit_code = Some(128);
    assert!(!failed.succeeded());
}
