//! The New Repository dialog: a name and folder, the first branch, and
//! starting files (a `.gitignore`, a README and a license).

use std::path::PathBuf;

use bonsai_git::{Git, IgnoreTemplate, License, NewRepository, Repository, is_empty_or_missing};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _,
    PathPromptOptions, Render, SharedString, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::ui::{
    button::{Button, ButtonSize, ButtonVariant, style_button},
    checkbox::Checkbox,
    dialog::{dialog_description, dialog_footer, dialog_title},
    input::{Input, InputEvent, InputState},
    menu::{Menu, MenuItem, MenuState},
    theme::UiTheme,
    toggle_group::{ToggleGroup, ToggleGroupItem},
};

/// What the dialog tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewRepoEvent {
    /// The repository was created here; open it and make the first commit
    /// when `commit` is set.
    Created {
        path: PathBuf,
        commit: bool,
    },
    /// The user chose another parent folder; remember it.
    ParentChanged(PathBuf),
    Dismissed,
}

pub struct NewRepoDialog {
    git: Git,
    name: Entity<InputState>,
    branch: Entity<InputState>,
    parent: PathBuf,
    ignore: IgnoreTemplate,
    ignore_menu: Entity<MenuState>,
    readme: bool,
    license: License,
    /// The license's copyright holder: the global `user.name`.
    holder: String,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<NewRepoEvent> for NewRepoDialog {}

/// The current year, for a license's copyright line.
fn current_year() -> i32 {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    i32::try_from(year).unwrap_or(2026)
}

impl NewRepoDialog {
    pub fn new(git: Git, parent: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("my-project"));
        let branch = cx.new(|cx| InputState::new(window, cx).placeholder("main"));
        branch.update(cx, |input, cx| input.set_value("main", window, cx));
        let holder = git
            .run_unchecked(&git.command(["config", "--global", "user.name"]))
            .map(|output| output.stdout_lossy().trim().to_owned())
            .unwrap_or_default();
        let ignore_menu = cx.new(|cx| MenuState::new(Vec::new(), cx).width(px(220.)));
        let subscriptions = vec![
            cx.subscribe_in(&name, window, |this, _, event, _, cx| match event {
                InputEvent::Change => {
                    this.error = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.create(cx),
                _ => {}
            }),
            cx.subscribe_in(&branch, window, |this, _, event, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.create(cx);
                }
            }),
        ];
        name.update(cx, |name, cx| name.focus(window, cx));
        let mut this = Self {
            git,
            name,
            branch,
            parent,
            ignore: IgnoreTemplate::MacOs,
            ignore_menu,
            readme: true,
            license: License::None,
            holder,
            error: None,
            _subscriptions: subscriptions,
        };
        this.rebuild_ignore_menu(cx);
        this
    }

    /// Sets the folder name, as typing would.
    pub fn set_name(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.name
            .update(cx, |input, cx| input.set_value(name.to_owned(), window, cx));
        cx.notify();
    }

    /// Puts the cursor in the name field.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.name.update(cx, |name, cx| name.focus(window, cx));
    }

    fn rebuild_ignore_menu(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let items: Vec<MenuItem> = IgnoreTemplate::ALL
            .iter()
            .enumerate()
            .map(|(i, template)| {
                let template = *template;
                let this = this.clone();
                MenuItem::new(("new-repo.ignore", i), template.name())
                    .radio(template == self.ignore)
                    .on_click(move |_, _, cx| {
                        let _ = this.update(cx, |this, cx| {
                            this.ignore = template;
                            this.rebuild_ignore_menu(cx);
                            cx.notify();
                        });
                    })
            })
            .collect();
        self.ignore_menu
            .update(cx, |menu, cx| menu.set_items(items, cx));
    }

    /// What would be created, or why it can't be.
    pub fn plan(&self, cx: &gpui_kit::App) -> Result<NewRepository, &'static str> {
        let name = self.name.read(cx).value();
        let name = name.trim();
        if name.is_empty() {
            return Err("Enter a name for the repository's folder.");
        }
        if name.contains('/') || name == "." || name == ".." {
            return Err("The name can't contain a slash.");
        }
        let path = self.parent.join(name);
        if !is_empty_or_missing(&path) {
            return Err("A folder with this name and files in it already exists there.");
        }
        let branch = self.branch.read(cx).value().trim().to_owned();
        if !branch.is_empty() && !bonsai_git::is_valid_ref_name(&branch) {
            return Err("That isn't a valid branch name.");
        }
        Ok(NewRepository {
            path,
            branch: Some(branch).filter(|b| !b.is_empty()),
            ignore: self.ignore,
            readme: self.readme,
            license: self.license,
            holder: if self.holder.is_empty() {
                name.to_owned()
            } else {
                self.holder.clone()
            },
            year: current_year(),
        })
    }

    /// Creates the repository when the form is valid.
    pub fn create(&mut self, cx: &mut Context<Self>) {
        let Ok(new) = self.plan(cx) else {
            return;
        };
        match Repository::create(&self.git, &new) {
            Ok(repository) => {
                let commit = !new.files().is_empty();
                cx.emit(NewRepoEvent::Created {
                    path: repository.root().to_path_buf(),
                    commit,
                });
            }
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
            }
        }
    }

    fn choose_parent(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(parent) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.parent.clone_from(&parent);
                this.error = None;
                cx.emit(NewRepoEvent::ParentChanged(parent));
                cx.notify();
            });
        })
        .detach();
    }

    fn license_group(&self, cx: &mut Context<Self>) -> ToggleGroup {
        let selected = License::ALL
            .iter()
            .position(|l| *l == self.license)
            .map(|i| SharedString::from(i.to_string()));
        let mut group = ToggleGroup::new("new-repo.license")
            .aria_label("License")
            .segmented(true)
            .value(selected);
        for (i, license) in License::ALL.iter().enumerate() {
            group = group.item(
                ToggleGroupItem::new(
                    SharedString::from(format!("new-repo.license.{i}")),
                    i.to_string(),
                )
                .aria_label(license.name())
                .child(license.name()),
            );
        }
        let this = cx.entity().downgrade();
        group.on_change(move |next, _, cx| {
            let Some(license) = next
                .first()
                .and_then(|i| i.parse::<usize>().ok())
                .and_then(|i| License::ALL.get(i).copied())
            else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.license = license;
                cx.notify();
            });
        })
    }
}

impl Render for NewRepoDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let plan = self.plan(cx);
        let field_label = |text: &'static str| {
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(text)
        };
        let field = |label: &'static str| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(field_label(label))
        };
        let hint = match (&self.error, &plan) {
            (Some(error), _) => Some((error.clone(), c.destructive)),
            (None, Err(reason)) if !self.name.read(cx).value().is_empty() => {
                Some((SharedString::from(*reason), c.muted_foreground))
            }
            _ => None,
        };
        let ignore_theme = theme.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(dialog_title("new-repo.title", cx).child("New Repository"))
            .child(dialog_description("new-repo.description", cx).child(
                "Starts a repository on this Mac. Publish it to GitHub, GitLab or Bitbucket whenever you're ready.",
            ))
            .child(field("Name").child(Input::new(&self.name).aria_label("Name")))
            .child(
                field("Create in").child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .text_color(c.muted_foreground)
                                .child(self.parent.to_string_lossy().into_owned()),
                        )
                        .child(
                            Button::new("new-repo.choose")
                                .variant(ButtonVariant::Outline)
                                .label("Choose…")
                                .on_click(cx.listener(|this, _, _, cx| this.choose_parent(cx))),
                        ),
                ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(12.))
                    .child(
                        field("First branch")
                            .flex_1()
                            .child(Input::new(&self.branch).aria_label("First branch")),
                    )
                    .child(
                        field(".gitignore").flex_1().child(
                            Menu::new(&self.ignore_menu, ".gitignore template")
                                .trigger_style_with(move |button| {
                                    style_button(
                                        button,
                                        false,
                                        ButtonVariant::Outline,
                                        ButtonSize::Default,
                                        &ignore_theme,
                                    )
                                })
                                .trigger(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .child(self.ignore.name())
                                        .child(
                                            lucide(LucideIcon::ChevronDown)
                                                .size(px(14.))
                                                .text_color(c.muted_foreground),
                                        ),
                                ),
                        ),
                    ),
            )
            .child(field("License").child(div().flex().child(self.license_group(cx))))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_sm()
                    .child(
                        Checkbox::new("new-repo.readme")
                            .checked(self.readme)
                            .aria_label("Add a README")
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.readme = *checked;
                                cx.notify();
                            })),
                    )
                    .child("Add a README"),
            )
            .when_some(hint, |this, (text, color)| {
                this.child(div().text_sm().text_color(color).child(text))
            })
            .child(
                dialog_footer(cx)
                    .child(
                        Button::new("new-repo.cancel")
                            .variant(ButtonVariant::Outline)
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NewRepoEvent::Dismissed))),
                    )
                    .child(
                        Button::new("new-repo.create")
                            .label("Create")
                            .disabled(plan.is_err())
                            .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn knows_the_year() {
        let year = super::current_year();
        assert!((2026..2100).contains(&year), "{year}");
    }
}
