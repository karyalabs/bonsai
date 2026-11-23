//! Every user command in one table: its action, key binding, menu bar
//! placement and command palette entry come from here (ADR 5).

use gpui_kit::{Action, App, KeyBinding, Menu, MenuItem, SharedString, actions};

actions!(
    bonsai,
    [
        Quit,
        OpenRepository,
        NewRepository,
        CloneRepository,
        ShowWelcome,
        ManageAccounts,
        ShowSettings,
        OpenInTerminal,
        CloseTab,
        NextTab,
        PreviousTab,
        ShowCommandPalette,
        ToggleSidebar,
        ShowChanges,
        ShowHistory,
        ShowFiles,
        ShowCompare,
        ShowReflog,
        ShowActivity,
        ShowPullRequests,
        ShowIssues,
        Refresh,
        Undo,
        Redo,
        Commit,
        Fetch,
        Pull,
        Push,
        PurgeFromHistory,
        ScanHistory,
        StartBisect,
        AddSubmodule,
        PublishRepository,
        CreatePullRequest,
        NewIssue,
        StashChanges,
        SwitchBranch,
        CommitAndPush,
        NewBranch,
    ]
);

/// Where a command sits in the menu bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuPlacement {
    App,
    File,
    Edit,
    View,
    Repository,
    Window,
}

impl MenuPlacement {
    const ORDER: [Self; 6] = [
        Self::App,
        Self::File,
        Self::Edit,
        Self::View,
        Self::Repository,
        Self::Window,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::App => "Bonsai",
            Self::File => "File",
            Self::Edit => "Edit",
            Self::View => "View",
            Self::Repository => "Repository",
            Self::Window => "Window",
        }
    }
}

/// One user command.
pub struct CommandSpec {
    /// Stable ID, used as the palette value.
    pub id: &'static str,
    pub label: &'static str,
    /// Palette group heading.
    pub group: &'static str,
    /// A GPUI keystroke source; `secondary-` is Cmd on macOS, Ctrl elsewhere.
    pub keys: &'static [&'static str],
    pub menu: Option<MenuPlacement>,
    /// Start a new menu section before this command.
    pub separator_before: bool,
    pub palette: bool,
    /// Extra palette search terms.
    pub keywords: &'static [&'static str],
    pub action: fn() -> Box<dyn Action>,
}

macro_rules! command {
    ($action:ident, $id:literal, $label:literal, $group:literal, keys: [$($key:literal),*], menu: $menu:expr, palette: $palette:literal $(, separator: $sep:literal)? $(, keywords: [$($kw:literal),*])?) => {
        CommandSpec {
            id: $id,
            label: $label,
            group: $group,
            keys: &[$($key),*],
            menu: $menu,
            separator_before: false $(|| $sep)?,
            palette: $palette,
            keywords: &[$($($kw),*)?],
            action: || Box::new($action),
        }
    };
}

/// All commands, in menu order.
pub const COMMANDS: &[CommandSpec] = &[
    command!(Quit, "app.quit", "Quit Bonsai", "Bonsai", keys: ["secondary-q"], menu: Some(MenuPlacement::App), palette: false),
    command!(OpenRepository, "file.open", "Open Repository…", "Repository", keys: ["secondary-o"], menu: Some(MenuPlacement::File), palette: true, keywords: ["add", "folder"]),
    command!(NewRepository, "file.new", "New Repository…", "Repository", keys: ["secondary-n"], menu: Some(MenuPlacement::File), palette: true, keywords: ["init", "create"]),
    command!(CloneRepository, "file.clone", "Clone Repository…", "Repository", keys: ["secondary-shift-o"], menu: Some(MenuPlacement::File), palette: true, keywords: ["download", "url", "remote"]),
    command!(ShowSettings, "app.settings", "Settings…", "Bonsai", keys: ["secondary-,"], menu: Some(MenuPlacement::App), palette: true, keywords: ["preferences", "options", "appearance", "theme", "editor", "git path"]),
    command!(ManageAccounts, "app.accounts", "Accounts…", "Bonsai", keys: [], menu: Some(MenuPlacement::App), palette: true, keywords: ["sign in", "github", "gitlab", "bitbucket", "token", "login"]),
    command!(ShowWelcome, "file.new-tab", "New Tab", "Window", keys: ["secondary-t"], menu: Some(MenuPlacement::File), palette: true, keywords: ["welcome", "recent"]),
    command!(CloseTab, "file.close-tab", "Close Tab", "Window", keys: ["secondary-w"], menu: Some(MenuPlacement::File), palette: true, separator: true),
    command!(Undo, "edit.undo", "Undo", "Edit", keys: ["secondary-z"], menu: Some(MenuPlacement::Edit), palette: true, keywords: ["revert", "back", "uncommit", "restore", "take back"]),
    command!(Redo, "edit.redo", "Redo", "Edit", keys: ["secondary-shift-z"], menu: Some(MenuPlacement::Edit), palette: true, keywords: ["again", "repeat"]),
    command!(ShowCommandPalette, "view.command-palette", "Command Palette…", "View", keys: ["secondary-shift-p", "secondary-k"], menu: Some(MenuPlacement::View), palette: false),
    command!(ToggleSidebar, "view.toggle-sidebar", "Toggle Sidebar", "View", keys: ["secondary-ctrl-s"], menu: Some(MenuPlacement::View), palette: true, separator: true),
    command!(ShowChanges, "view.changes", "Show Changes", "View", keys: ["secondary-1"], menu: Some(MenuPlacement::View), palette: true, separator: true, keywords: ["status", "working tree", "stage"]),
    command!(ShowHistory, "view.history", "Show History", "View", keys: ["secondary-2"], menu: Some(MenuPlacement::View), palette: true, keywords: ["log", "commits", "graph"]),
    command!(ShowFiles, "view.files", "Show Files", "View", keys: ["secondary-6"], menu: Some(MenuPlacement::View), palette: true, keywords: ["explorer", "browse", "tree", "tracked"]),
    command!(ShowCompare, "view.compare", "Compare Branches", "View", keys: ["secondary-8"], menu: Some(MenuPlacement::View), palette: true, keywords: ["diff", "ahead", "behind", "branches", "range"]),
    command!(ShowReflog, "view.reflog", "Show Reflog", "View", keys: ["secondary-7"], menu: Some(MenuPlacement::View), palette: true, keywords: ["recover", "lost commits", "undo", "restore", "reset"]),
    command!(ShowActivity, "view.activity", "Show Activity", "View", keys: ["secondary-3"], menu: Some(MenuPlacement::View), palette: true, keywords: ["log", "commands", "output", "errors"]),
    command!(ShowPullRequests, "view.pull-requests", "Show Pull Requests", "View", keys: ["secondary-4"], menu: Some(MenuPlacement::View), palette: true, keywords: ["pr", "merge requests", "review", "github", "gitlab", "bitbucket"]),
    command!(ShowIssues, "view.issues", "Show Issues", "View", keys: ["secondary-5"], menu: Some(MenuPlacement::View), palette: true, keywords: ["bugs", "tickets", "github", "gitlab", "bitbucket"]),
    command!(Commit, "repository.commit", "Commit", "Repository", keys: ["secondary-enter"], menu: Some(MenuPlacement::Repository), palette: true, keywords: ["save", "amend"]),
    command!(CommitAndPush, "repository.commit-push", "Commit and Push", "Repository", keys: ["secondary-shift-enter"], menu: Some(MenuPlacement::Repository), palette: true, keywords: ["save", "upload"]),
    command!(Fetch, "repository.fetch", "Fetch", "Repository", keys: ["secondary-alt-f"], menu: Some(MenuPlacement::Repository), palette: true, separator: true, keywords: ["download", "update", "remote"]),
    command!(Pull, "repository.pull", "Pull", "Repository", keys: ["secondary-alt-l"], menu: Some(MenuPlacement::Repository), palette: true, keywords: ["download", "merge", "rebase", "update"]),
    command!(Push, "repository.push", "Push", "Repository", keys: ["secondary-alt-p"], menu: Some(MenuPlacement::Repository), palette: true, keywords: ["upload", "publish", "remote"]),
    command!(SwitchBranch, "repository.switch-branch", "Switch Branch…", "Repository", keys: ["secondary-b"], menu: Some(MenuPlacement::Repository), palette: true, separator: true, keywords: ["checkout", "branch", "change"]),
    command!(NewBranch, "repository.new-branch", "New Branch…", "Repository", keys: ["secondary-shift-b"], menu: Some(MenuPlacement::Repository), palette: true, keywords: ["create", "branch"]),
    command!(StashChanges, "repository.stash", "Stash Changes…", "Repository", keys: ["secondary-alt-s"], menu: Some(MenuPlacement::Repository), palette: true, keywords: ["save", "shelve", "set aside"]),
    command!(PublishRepository, "repository.publish", "Publish Repository…", "Repository", keys: [], menu: Some(MenuPlacement::Repository), palette: true, keywords: ["github", "gitlab", "bitbucket", "create", "upload", "remote", "origin"]),
    command!(CreatePullRequest, "repository.create-pull-request", "New Pull Request…", "Repository", keys: [], menu: Some(MenuPlacement::Repository), palette: true, keywords: ["pr", "merge request", "review", "open", "github", "gitlab", "bitbucket"]),
    command!(NewIssue, "repository.new-issue", "New Issue…", "Repository", keys: [], menu: Some(MenuPlacement::Repository), palette: true, keywords: ["bug", "ticket", "report", "github", "gitlab", "bitbucket"]),
    command!(PurgeFromHistory, "repository.purge", "Purge from History…", "Repository", keys: [], menu: Some(MenuPlacement::Repository), palette: true, separator: true, keywords: ["remove", "secret", "env", "rewrite", "filter", "delete file"]),
    command!(ScanHistory, "repository.scan-history", "Find Secrets and Large Files…", "Repository", keys: [], menu: Some(MenuPlacement::Repository), palette: true, separator: false, keywords: ["secret", "credential", "token", "key", "leak", "large", "big", "size", "purge"]),
    command!(StartBisect, "repository.bisect", "Find a Bad Commit (Bisect)…", "Repository", keys: [], menu: Some(MenuPlacement::Repository), palette: true, separator: false, keywords: ["bisect", "regression", "bug", "broke", "search"]),
    command!(AddSubmodule, "repository.add-submodule", "Add Submodule…", "Repository", keys: [], menu: Some(MenuPlacement::Repository), palette: true, separator: false, keywords: ["submodule", "vendor", "dependency", "nested"]),
    command!(OpenInTerminal, "repository.terminal", "Open in Terminal", "Repository", keys: ["secondary-alt-t"], menu: Some(MenuPlacement::Repository), palette: true, keywords: ["shell", "console", "command line"]),
    command!(Refresh, "repository.refresh", "Refresh", "Repository", keys: ["secondary-r"], menu: Some(MenuPlacement::Repository), palette: true, keywords: ["reload", "status"]),
    command!(NextTab, "window.next-tab", "Show Next Tab", "Window", keys: ["ctrl-tab", "secondary-shift-]"], menu: Some(MenuPlacement::Window), palette: true),
    command!(PreviousTab, "window.previous-tab", "Show Previous Tab", "Window", keys: ["ctrl-shift-tab", "secondary-shift-["], menu: Some(MenuPlacement::Window), palette: true),
];

/// Looks up a command by its palette value.
#[must_use]
pub fn find(id: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|command| command.id == id)
}

/// Binds every command's keys and installs the menu bar.
pub fn install(cx: &mut App) {
    let bindings: Vec<KeyBinding> = COMMANDS
        .iter()
        .flat_map(|command| {
            command.keys.iter().map(|keys| {
                KeyBinding::load(keys, (command.action)(), None, false, None, cx).unwrap_or_else(
                    |error| panic!("{} has a bad key binding: {error:?}", command.id),
                )
            })
        })
        .collect();
    cx.bind_keys(bindings);
    cx.set_menus(menus());
}

fn menus() -> Vec<Menu> {
    MenuPlacement::ORDER
        .into_iter()
        .map(|placement| {
            let mut items = Vec::new();
            for command in COMMANDS.iter().filter(|c| c.menu == Some(placement)) {
                if command.separator_before && !items.is_empty() {
                    items.push(MenuItem::separator());
                }
                items.push(MenuItem::Action {
                    name: SharedString::new_static(command.label),
                    action: (command.action)(),
                    os_action: None,
                    checked: false,
                    disabled: false,
                });
            }
            Menu::new(placement.title()).items(items)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn ids_and_keys_are_unique() {
        let mut ids = HashSet::new();
        let mut keys = HashSet::new();
        for command in COMMANDS {
            assert!(ids.insert(command.id), "duplicate id {}", command.id);
            for key in command.keys {
                assert!(keys.insert(*key), "duplicate key {key}");
            }
        }
    }

    #[test]
    fn every_command_is_reachable_from_the_menu_or_the_palette() {
        for command in COMMANDS {
            assert!(
                command.menu.is_some() || command.palette,
                "{} is in neither the menu bar nor the palette",
                command.id
            );
        }
    }

    #[test]
    fn every_menu_command_appears_in_the_menu_bar() {
        let menus = menus();
        let names: Vec<String> = menus
            .iter()
            .flat_map(|menu| &menu.items)
            .filter_map(|item| match item {
                MenuItem::Action { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .collect();
        for command in COMMANDS.iter().filter(|c| c.menu.is_some()) {
            assert!(
                names.iter().any(|n| n == command.label),
                "{} missing",
                command.id
            );
        }
    }

    #[gpui_kit::test]
    fn every_binding_parses(cx: &mut gpui_kit::TestAppContext) {
        cx.update(install);
    }
}
