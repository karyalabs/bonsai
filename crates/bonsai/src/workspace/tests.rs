use bonsai_git::{Git, Repository};
use gpui_kit::{TestAppContext, VisualTestContext};

use super::{Section, Workspace};
use crate::{commands, session::Session};

fn repository(parent: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = parent.join(name);
    Repository::init(&Git::locate(None).unwrap(), &path, Some("main")).unwrap();
    path
}

fn setup(
    cx: &mut TestAppContext,
    session: Session,
    session_path: Option<std::path::PathBuf>,
) -> (gpui_kit::Entity<Workspace>, &mut VisualTestContext) {
    cx.update(|cx| {
        crate::ui::theme::init(cx);
        commands::install(cx);
    });
    let (workspace, cx) =
        cx.add_window_view(|window, cx| Workspace::new(session, session_path, false, window, cx));
    cx.run_until_parked();
    (workspace, cx)
}

#[gpui_kit::test]
fn opens_tabs_once_and_saves_them(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let alpha = repository(dir.path(), "alpha");
    let beta = repository(dir.path(), "beta");
    let session_path = dir.path().join("session.json");
    let (workspace, cx) = setup(cx, Session::default(), Some(session_path.clone()));

    workspace.update(cx, |workspace, cx| {
        workspace.open_path(&alpha, true, cx).unwrap();
        workspace.open_path(&beta.join("."), true, cx).unwrap();
        workspace.open_path(&alpha, true, cx).unwrap();
        workspace.sync_session_tabs(cx);
    });
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.tab_titles(), ["alpha", "beta"]);
        assert_eq!(
            workspace.active_index(),
            0,
            "reopening switches to the existing tab"
        );
    });
    let saved = Session::load(&session_path);
    assert_eq!(saved.tabs.len(), 2);
    assert_eq!(saved.active_tab, 0);
}

#[gpui_kit::test]
fn restores_tabs_and_skips_missing_ones(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let alpha = repository(dir.path(), "alpha");
    let beta = repository(dir.path(), "beta");
    let session = Session {
        tabs: vec![alpha, dir.path().join("gone"), beta],
        active_tab: 1,
        ..Session::default()
    };
    let (workspace, cx) = setup(cx, session, None);
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.tab_titles(), ["alpha", "beta"]);
        assert_eq!(workspace.active_index(), 1);
    });
}

#[gpui_kit::test]
fn keyboard_commands_switch_tabs_sections_and_close(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let session = Session {
        tabs: vec![
            repository(dir.path(), "a"),
            repository(dir.path(), "b"),
            repository(dir.path(), "c"),
        ],
        ..Session::default()
    };
    let (workspace, cx) = setup(cx, session, None);

    cx.dispatch_action(commands::NextTab);
    cx.dispatch_action(commands::NextTab);
    workspace.read_with(cx, |w, _| assert_eq!(w.active_index(), 2));
    cx.dispatch_action(commands::NextTab);
    workspace.read_with(cx, |w, _| assert_eq!(w.active_index(), 0, "wraps around"));
    cx.dispatch_action(commands::PreviousTab);
    workspace.read_with(cx, |w, _| assert_eq!(w.active_index(), 2));

    cx.dispatch_action(commands::ShowHistory);
    workspace.read_with(cx, |w, _| assert_eq!(w.section(), Section::History));

    cx.dispatch_action(commands::ToggleSidebar);
    workspace.read_with(cx, |w, _| assert!(!w.session().sidebar_open));

    cx.dispatch_action(commands::CloseTab);
    workspace.read_with(cx, |w, _| {
        assert_eq!(w.tab_titles(), ["a", "b"]);
        assert_eq!(w.active_index(), 1);
    });
}

#[gpui_kit::test]
fn the_command_palette_opens(cx: &mut TestAppContext) {
    let (workspace, cx) = setup(cx, Session::default(), None);
    cx.dispatch_action(commands::ShowCommandPalette);
    cx.run_until_parked();
    workspace.read_with(cx, |w, _| assert!(w.palette.is_open()));
}

#[test]
fn every_palette_command_is_handled_by_the_workspace() {
    // Quit is handled by the app; everything else must have a listener in
    // `Workspace::render`. Keep this list in step with the `on_action` calls.
    let handled = [
        "file.open",
        "file.new",
        "file.clone",
        "file.new-tab",
        "file.close-tab",
        "window.next-tab",
        "window.previous-tab",
        "view.command-palette",
        "view.toggle-sidebar",
        "view.changes",
        "view.history",
        "repository.refresh",
    ];
    for command in commands::COMMANDS {
        assert!(
            command.id == "app.quit" || handled.contains(&command.id),
            "{} has no handler",
            command.id
        );
    }
}

#[gpui_kit::test]
fn opening_remembers_recents_and_new_tab_shows_welcome(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let alpha = repository(dir.path(), "alpha");
    let beta = repository(dir.path(), "beta");
    let session_path = dir.path().join("session.json");
    let (workspace, cx) = setup(cx, Session::default(), Some(session_path.clone()));

    workspace.update(cx, |workspace, cx| {
        workspace.open_paths(&[alpha.clone(), beta.clone()], cx);
    });
    let saved = Session::load(&session_path);
    assert_eq!(saved.recents.len(), 2);
    assert!(saved.recents[0].ends_with("beta"));

    cx.dispatch_action(commands::ShowWelcome);
    workspace.read_with(cx, |workspace, _| assert!(workspace.is_showing_welcome()));
    workspace.update(cx, |workspace, cx| workspace.open_recent(&alpha, cx));
    workspace.read_with(cx, |workspace, _| {
        assert!(!workspace.is_showing_welcome());
        assert_eq!(workspace.active_index(), 0);
    });
    assert!(Session::load(&session_path).recents[0].ends_with("alpha"));

    workspace.update(cx, |workspace, cx| {
        workspace.forget_recent(&saved.recents[0], cx);
    });
    assert_eq!(Session::load(&session_path).recents.len(), 1);
}

#[gpui_kit::test]
fn clones_from_a_url_and_opens_the_result(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let source = repository(dir.path(), "source");
    let git = Git::locate(None).unwrap();
    std::fs::write(source.join("README.md"), "hello\n").unwrap();
    for args in [
        vec!["add", "."],
        vec![
            "-c",
            "user.name=B",
            "-c",
            "user.email=b@example.com",
            "commit",
            "-qm",
            "first",
        ],
    ] {
        git.run(&git.command(args).cwd(&source)).unwrap();
    }
    let parent = dir.path().join("clones");
    std::fs::create_dir(&parent).unwrap();
    let session = Session {
        clone_parent: Some(parent.clone()),
        ..Session::default()
    };
    let (workspace, cx) = setup(cx, session, None);

    cx.dispatch_action(commands::CloneRepository);
    let dialog = workspace.read_with(cx, |workspace, _| {
        workspace.clone_dialog().cloned().unwrap()
    });
    let url = format!("file://{}", source.display());
    cx.update(|window, cx| dialog.update(cx, |dialog, cx| dialog.set_url(&url, window, cx)));
    let destination = dialog
        .read_with(cx, |dialog, cx| dialog.destination(cx))
        .unwrap();
    assert_eq!(destination, parent.join("source"));

    dialog.update(cx, |dialog, cx| dialog.start(cx));
    for _ in 0..200 {
        cx.run_until_parked();
        if workspace.read_with(cx, |workspace, _| workspace.tab_titles().len()) == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.tab_titles(), ["source"]);
        assert!(workspace.clone_dialog().is_none());
    });
    assert!(parent.join("source/README.md").exists());
}
