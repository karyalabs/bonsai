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
