//! The window's title bar: the sidebar toggle, the repository and branch
//! switchers, sync, and the command palette, beside the traffic lights.

use std::path::PathBuf;

use bonsai_git::Head;
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, prelude::FluentBuilder as _, px,
};

use super::{Workspace, tab_title};
use crate::{
    commands,
    ui::{
        button::{Button, ButtonSize, ButtonVariant, style_button},
        menu::{Menu, MenuItem},
        theme::UiTheme,
        title_bar::TitleBar,
        tooltip::text_tooltip,
    },
};

/// Height of Bonsai's title bar; the traffic lights are placed to sit in its
/// middle (see `lib.rs`).
pub const HEIGHT: f32 = 44.;
/// Room the macOS traffic lights take at the left of the bar.
const TRAFFIC_LIGHTS: f32 = 84.;

/// What the repository menu was last built from, so it is rebuilt only when
/// the tabs or recents change.
#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct RepoMenuKey {
    tabs: Vec<SharedString>,
    active: Option<usize>,
    recents: Vec<PathBuf>,
}

/// The current branch as the title bar shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchSummary {
    pub label: SharedString,
    pub detached: bool,
    pub ahead: u32,
    pub behind: u32,
    /// The upstream, such as `origin/main`; `None` when it has none.
    pub upstream: Option<SharedString>,
}

impl BranchSummary {
    #[must_use]
    pub fn from_snapshot(snapshot: &bonsai_git::Snapshot) -> Self {
        match &snapshot.head {
            Head::Branch { name, .. } => {
                let tracking = snapshot
                    .branches
                    .iter()
                    .find(|b| b.name == *name)
                    .and_then(|b| b.upstream.as_ref());
                Self {
                    label: name.clone().into(),
                    detached: false,
                    ahead: tracking.map_or(0, |t| t.ahead),
                    behind: tracking.map_or(0, |t| t.behind),
                    upstream: tracking.filter(|t| !t.gone).map(|t| t.name.clone().into()),
                }
            }
            Head::Detached { oid } => Self {
                label: oid.short().into(),
                detached: true,
                ..Self::default()
            },
        }
    }

    /// The tooltip on the branch button.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.detached {
            return format!("Detached at {}. Switch branch (⌘B)", self.label);
        }
        let sync = match (&self.upstream, self.ahead, self.behind) {
            (None, ..) => "No upstream".to_owned(),
            (Some(up), 0, 0) => format!("Up to date with {up}"),
            (Some(up), a, 0) => format!("{a} ahead of {up}"),
            (Some(up), 0, b) => format!("{b} behind {up}"),
            (Some(up), a, b) => format!("{a} ahead, {b} behind {up}"),
        };
        format!("{sync}. Switch branch (⌘B)")
    }
}

/// Styles a menu trigger as a small ghost button, for menus in toolbars.
pub fn ghost_trigger(
    theme: &UiTheme,
    size: ButtonSize,
) -> impl FnOnce(gpui_kit::base::Button) -> gpui_kit::base::Button + 'static {
    let theme = theme.clone();
    move |button| style_button(button, false, ButtonVariant::Ghost, size, &theme)
}

/// A small ghost button for the title bar, with a tooltip that also names
/// it for assistive technology.
pub fn bar_button(
    id: impl Into<gpui_kit::ElementId>,
    tip: impl Into<SharedString>,
    disabled: bool,
    theme: &UiTheme,
) -> Button {
    let tip: SharedString = tip.into();
    let theme = theme.clone();
    Button::new(id)
        .disabled(disabled)
        .aria_label(tip.clone())
        .style_with(move |button| {
            let tip = tip.clone();
            style_button(
                button,
                disabled,
                ButtonVariant::Ghost,
                ButtonSize::Sm,
                &theme,
            )
            .tooltip(move |_, cx| text_tooltip(tip.clone(), cx))
        })
}

impl Workspace {
    fn rebuild_repo_menu(&mut self, cx: &mut Context<Self>) {
        let active = (!self.welcome && !self.tabs.is_empty()).then_some(self.active);
        let key = RepoMenuKey {
            tabs: self.tabs.iter().map(|t| t.title.clone()).collect(),
            active,
            recents: self.session.recents.clone(),
        };
        if key == self.repo_menu_key {
            return;
        }
        let this = cx.entity().downgrade();
        let mut items = Vec::new();
        if !self.tabs.is_empty() {
            items.push(MenuItem::label("repo.open.label", "Open"));
        }
        for (index, tab) in self.tabs.iter().enumerate() {
            let this = this.clone();
            items.push(
                MenuItem::new(("repo.tab", tab.id), tab.title.clone())
                    .icon(LucideIcon::FolderGit2)
                    .checked(active == Some(index))
                    .on_click(move |_, _, cx| {
                        let _ = this.update(cx, |this, cx| this.select_tab(index, cx));
                    }),
            );
        }
        let open: Vec<PathBuf> = self
            .tabs
            .iter()
            .map(|t| t.model.read(cx).repository().root().to_path_buf())
            .collect();
        let recents: Vec<PathBuf> = self
            .session
            .recents
            .iter()
            .filter(|path| !open.contains(path))
            .take(8)
            .cloned()
            .collect();
        if !recents.is_empty() {
            items.push(MenuItem::separator());
            items.push(MenuItem::label("repo.recent.label", "Recent"));
            for (index, path) in recents.into_iter().enumerate() {
                let this = this.clone();
                let description = path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                items.push(
                    MenuItem::new(("repo.recent", index), tab_title(&path))
                        .icon(LucideIcon::Folder)
                        .description(description)
                        .on_click(move |_, _, cx| {
                            let _ = this.update(cx, |this, cx| this.open_recent(&path, cx));
                        }),
                );
            }
        }
        items.push(MenuItem::separator());
        let action = |id: &'static str,
                      label: &'static str,
                      icon: LucideIcon,
                      keys: &'static str,
                      action: fn() -> Box<dyn gpui_kit::Action>| {
            MenuItem::new(id, label)
                .icon(icon)
                .shortcut(keys)
                .on_click(move |_, window, cx| window.dispatch_action(action(), cx))
        };
        items.push(action(
            "repo.action.open",
            "Open Repository…",
            LucideIcon::FolderOpen,
            "secondary-o",
            || Box::new(commands::OpenRepository),
        ));
        items.push(action(
            "repo.action.clone",
            "Clone Repository…",
            LucideIcon::Download,
            "secondary-shift-o",
            || Box::new(commands::CloneRepository),
        ));
        items.push(action(
            "repo.action.new",
            "New Repository…",
            LucideIcon::FolderPlus,
            "secondary-n",
            || Box::new(commands::NewRepository),
        ));
        self.repo_menu
            .update(cx, |menu, cx| menu.set_items(items, cx));
        self.repo_menu_key = key;
    }

    pub(super) fn render_title_bar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.rebuild_repo_menu(cx);
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let icon = |name: LucideIcon| lucide(name).size(px(15.)).text_color(c.sidebar_foreground);
        let tab = self.tabs.get(self.active).filter(|_| !self.welcome);
        let repo_name: SharedString = tab.map_or_else(|| "Bonsai".into(), |t| t.title.clone());
        let branch = tab
            .and_then(|t| t.model.read(cx).snapshot())
            .map(BranchSummary::from_snapshot);

        let left_inset = if cfg!(target_os = "macos") && !window.is_fullscreen() {
            px(TRAFFIC_LIGHTS)
        } else {
            px(8.)
        };

        let repo_trigger = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(icon(LucideIcon::FolderGit2))
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(c.sidebar_accent_foreground)
                    .child(repo_name),
            )
            .child(
                lucide(LucideIcon::ChevronsUpDown)
                    .size(px(12.))
                    .text_color(c.muted_foreground),
            );

        let branch_button = branch.map(|branch| {
            let counts = div()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_xs()
                .text_color(c.muted_foreground)
                .when(branch.ahead > 0, |this| {
                    this.child(format!("↑{}", branch.ahead))
                })
                .when(branch.behind > 0, |this| {
                    this.child(format!("↓{}", branch.behind))
                });
            bar_button("titlebar.branch", branch.describe(), false, &theme)
                .on_click(cx.listener(|this, _, window, cx| this.show_branch_picker(window, cx)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(icon(if branch.detached {
                            LucideIcon::GitCommitHorizontal
                        } else {
                            LucideIcon::GitBranch
                        }))
                        .child(
                            div()
                                .max_w(px(220.))
                                .truncate()
                                .text_color(c.sidebar_accent_foreground)
                                .child(branch.label.clone()),
                        )
                        .child(counts)
                        .child(
                            lucide(LucideIcon::ChevronDown)
                                .size(px(12.))
                                .text_color(c.muted_foreground),
                        ),
                )
        });

        let sidebar_open = self.session.sidebar_open;
        let left = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .when(tab.is_some(), |this| {
                this.child(
                    bar_button(
                        "titlebar.sidebar",
                        if sidebar_open {
                            "Hide Sidebar (⌃⌘S)"
                        } else {
                            "Show Sidebar (⌃⌘S)"
                        },
                        false,
                        &theme,
                    )
                    .child(icon(if sidebar_open {
                        LucideIcon::PanelLeftClose
                    } else {
                        LucideIcon::PanelLeft
                    }))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(commands::ToggleSidebar), cx);
                    }),
                )
                .child(div().w(px(4.)))
            })
            .child(
                Menu::new(&self.repo_menu, "Repositories")
                    .trigger_style_with(ghost_trigger(&theme, ButtonSize::Sm))
                    .trigger(repo_trigger),
            )
            .when_some(branch_button, |this, button| {
                this.child(div().text_color(c.border).text_sm().px(px(2.)).child("/"))
                    .child(button)
            });

        let palette = bar_button("titlebar.palette", "Search Commands (⌘K)", false, &theme)
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(commands::ShowCommandPalette), cx);
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(icon(LucideIcon::Search))
                    .child(
                        div()
                            .px(px(5.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(c.border)
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child("⌘K"),
                    ),
            );

        TitleBar::new()
            .h(px(HEIGHT))
            .pl(left_inset)
            .pr(px(8.))
            .child(
                div()
                    .id("titlebar.content")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(left)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .when_some(tab, |this, tab| this.child(tab.sync.clone()))
                            .when(tab.is_some(), |this| {
                                this.child(div().w(px(1.)).h(px(18.)).mx(px(4.)).bg(c.border))
                            })
                            .child(palette),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_the_branch_and_its_upstream() {
        let mut branch = BranchSummary {
            label: "main".into(),
            upstream: Some("origin/main".into()),
            ..BranchSummary::default()
        };
        assert_eq!(
            branch.describe(),
            "Up to date with origin/main. Switch branch (⌘B)"
        );
        branch.ahead = 2;
        branch.behind = 1;
        assert_eq!(
            branch.describe(),
            "2 ahead, 1 behind origin/main. Switch branch (⌘B)"
        );
        branch.upstream = None;
        assert_eq!(branch.describe(), "No upstream. Switch branch (⌘B)");
    }
}
