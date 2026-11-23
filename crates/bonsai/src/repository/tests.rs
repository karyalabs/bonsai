use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use bonsai_git::{ChangeKind, Git, Repository};
use gpui_kit::{AppContext as _, TestAppContext};

use super::{RepositoryEvent, RepositoryModel};

fn new_repository() -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let git = Git::locate(None).unwrap();
    let repository = Repository::init(&git, dir.path(), Some("main")).unwrap();
    (dir, repository)
}

#[gpui_kit::test]
fn reads_the_first_snapshot(cx: &mut TestAppContext) {
    let (dir, repository) = new_repository();
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
    cx.run_until_parked();
    model.read_with(cx, |model, _| {
        let snapshot = model.snapshot().expect("snapshot loaded");
        assert_eq!(snapshot.changes.len(), 1);
        assert_eq!(snapshot.changes[0].kind, ChangeKind::Untracked);
        assert!(
            model.activity().len() >= 3,
            "the snapshot's commands are logged"
        );
    });
}

#[gpui_kit::test]
fn operations_run_in_order_and_refresh(cx: &mut TestAppContext) {
    let (dir, repository) = new_repository();
    let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
    let order = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorded = events.clone();
    cx.update(|cx| {
        cx.subscribe(&model, move |_, event: &RepositoryEvent, _| {
            recorded.lock().unwrap().push(event.clone());
        })
        .detach();
    });

    let root = dir.path().to_path_buf();
    model.update(cx, |model, cx| {
        for name in ["one", "two", "three"] {
            let order = order.clone();
            let root = root.clone();
            model.run(
                name,
                move |_| {
                    order.lock().unwrap().push(name);
                    std::fs::write(root.join(format!("{name}.txt")), name).unwrap();
                    Ok(())
                },
                cx,
            );
        }
        assert_eq!(model.running().map(AsRef::as_ref), Some("one"));
        assert_eq!(model.queued(), 2);
    });
    cx.run_until_parked();

    assert_eq!(*order.lock().unwrap(), ["one", "two", "three"]);
    model.read_with(cx, |model, _| {
        assert!(model.running().is_none());
        assert_eq!(model.snapshot().unwrap().changes.len(), 3);
    });
    let finished: Vec<_> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            RepositoryEvent::OperationFinished { name, error, .. } => {
                assert!(error.is_none());
                Some(name.to_string())
            }
            RepositoryEvent::SnapshotChanged => None,
        })
        .collect();
    assert_eq!(finished, ["one", "two", "three"]);
}

#[gpui_kit::test]
fn a_failed_operation_reports_its_error_and_the_queue_continues(cx: &mut TestAppContext) {
    let (_dir, repository) = new_repository();
    let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let recorded = errors.clone();
    cx.update(|cx| {
        cx.subscribe(&model, move |_, event: &RepositoryEvent, _| {
            if let RepositoryEvent::OperationFinished { name, error, .. } = event {
                recorded
                    .lock()
                    .unwrap()
                    .push((name.to_string(), error.clone()));
            }
        })
        .detach();
    });
    model.update(cx, |model, cx| {
        model.run(
            "checkout missing",
            |repo| {
                let git = repo.git();
                git.run(&git.command(["checkout", "no-such-branch"]).cwd(repo.root()))
                    .map(drop)
            },
            cx,
        );
        model.run("after", |_| Ok(()), cx);
    });
    cx.run_until_parked();
    let errors = errors.lock().unwrap();
    assert_eq!(errors.len(), 2);
    assert!(
        errors[0]
            .1
            .as_ref()
            .is_some_and(|e| e.contains("no-such-branch"))
    );
    assert_eq!(errors[1], ("after".to_owned(), None));
}

#[gpui_kit::test]
fn a_refresh_during_an_operation_waits_for_it(cx: &mut TestAppContext) {
    let (dir, repository) = new_repository();
    let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
    cx.run_until_parked();
    let root = dir.path().to_path_buf();
    model.update(cx, |model, cx| {
        model.run(
            "write",
            move |_| {
                std::fs::write(root.join("x.txt"), "x").unwrap();
                Ok(())
            },
            cx,
        );
        model.refresh(cx);
    });
    cx.run_until_parked();
    model.read_with(cx, |model, _| {
        assert_eq!(
            model.snapshot().unwrap().changes[0].path,
            Path::new("x.txt")
        );
    });
}

#[gpui_kit::test]
fn a_background_fetch_is_quiet_and_waits_its_turn(cx: &mut TestAppContext) {
    let (dir, repository) = new_repository();
    let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
    cx.run_until_parked();
    // Without a remote there is nothing to fetch.
    assert!(!model.update(cx, RepositoryModel::fetch_in_background));

    let missing = dir.path().join("no-such-remote");
    model.update(cx, |model, cx| {
        let url = missing.display().to_string();
        model.run(
            "add remote",
            move |repo| repo.add_remote("origin", &url),
            cx,
        );
        // Something is running, so the background fetch steps aside.
        assert!(!model.fetch_in_background(cx));
    });
    cx.run_until_parked();

    let finished = Arc::new(Mutex::new(Vec::new()));
    let recorded = finished.clone();
    cx.update(|cx| {
        cx.subscribe(&model, move |_, event: &RepositoryEvent, _| {
            if let RepositoryEvent::OperationFinished { error, quiet, .. } = event {
                recorded.lock().unwrap().push((error.is_some(), *quiet));
            }
        })
        .detach();
    });
    assert!(model.update(cx, RepositoryModel::fetch_in_background));
    cx.run_until_parked();
    assert_eq!(*finished.lock().unwrap(), [(true, true)]);
}

fn committed_repository() -> (tempfile::TempDir, Repository) {
    let (dir, repository) = new_repository();
    let git = repository.git().clone();
    let run = |args: &[&str]| {
        git.run(&git.command(args.iter().copied()).cwd(dir.path()))
            .unwrap();
    };
    for (key, value) in [
        ("user.name", "Ada Lovelace"),
        ("user.email", "ada@example.com"),
        ("commit.gpgsign", "false"),
    ] {
        run(&["config", key, value]);
    }
    std::fs::write(dir.path().join("a.txt"), "base\n").unwrap();
    run(&["add", "a.txt"]);
    run(&["commit", "-qm", "base"]);
    (dir, repository)
}

#[gpui_kit::test]
fn undoes_and_redoes_a_commit_and_a_discard(cx: &mut TestAppContext) {
    use super::UndoNote::{None as Nothing, Redoable, Undoable};
    let (dir, repository) = committed_repository();
    let root = dir.path().to_path_buf();
    let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
    let notes = Arc::new(Mutex::new(Vec::new()));
    let recorded = notes.clone();
    cx.update(|cx| {
        cx.subscribe(&model, move |_, event: &RepositoryEvent, _| {
            if let RepositoryEvent::OperationFinished { name, undo, .. } = event {
                recorded
                    .lock()
                    .unwrap()
                    .push((name.to_string(), undo.clone()));
            }
        })
        .detach();
    });
    cx.run_until_parked();

    std::fs::write(root.join("a.txt"), "edited\n").unwrap();
    model.update(cx, |model, cx| {
        model.run("Stage a.txt", |r| r.stage(&["a.txt".into()]), cx);
        model.run("Commit", |r| r.commit("Edit a", false).map(|_| ()), cx);
    });
    cx.run_until_parked();
    model.read_with(cx, |model, _| assert_eq!(model.undo_name(), Some("Commit")));

    model.update(cx, |model, cx| assert!(model.undo(cx)));
    cx.run_until_parked();
    model.read_with(cx, |model, _| {
        let snapshot = model.snapshot().unwrap();
        assert_eq!(
            snapshot.changes.len(),
            1,
            "the commit's change is staged again"
        );
        assert!(snapshot.changes[0].is_staged());
        assert_eq!(model.redo_name(), Some("Commit"));
        assert_eq!(model.undo_name(), None);
    });
    model.update(cx, |model, cx| assert!(model.redo(cx)));
    cx.run_until_parked();
    model.read_with(cx, |model, _| {
        assert_eq!(model.snapshot().unwrap().changes.len(), 0);
        assert_eq!(model.undo_name(), Some("Commit"));
    });

    std::fs::write(root.join("a.txt"), "precious\n").unwrap();
    model.update(cx, |model, cx| {
        model.run_with(
            "Discard a.txt",
            super::UndoHints {
                backup: vec!["a.txt".into()],
                reset: None,
            },
            |r| r.discard(&["a.txt".into()]),
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "edited\n"
    );
    model.update(cx, |model, cx| assert!(model.undo(cx)));
    cx.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "precious\n"
    );

    assert_eq!(
        *notes.lock().unwrap(),
        [
            ("Stage a.txt".to_owned(), Nothing),
            ("Commit".to_owned(), Undoable),
            ("Undo Commit".to_owned(), Redoable),
            ("Redo Commit".to_owned(), Undoable),
            ("Discard a.txt".to_owned(), Undoable),
            ("Undo Discard a.txt".to_owned(), Redoable),
        ]
    );
}
