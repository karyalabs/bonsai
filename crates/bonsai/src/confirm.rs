//! The one way Bonsai asks before an action it can't undo (#31): a dialog
//! that says what will happen, with Cancel first and the action in red.

use gpui_kit::{App, ClickEvent, Context, Div, ParentElement as _, SharedString, Stateful, Window};

use crate::ui::{
    button::{Button, ButtonVariant},
    dialog::{dialog_description, dialog_footer, dialog_popup, dialog_title},
};

type Handler<V> = Box<dyn Fn(&mut V, &mut Window, &mut Context<V>)>;

/// A confirmation's popup: its title and what will happen. Callers add any
/// fields they need, then [`Confirm::footer`]. Element ids start with `id`.
pub fn popup(
    id: &str,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    cx: &App,
) -> Stateful<Div> {
    let title = title.into();
    dialog_popup(SharedString::from(format!("{id}.popup")), title.clone(), cx)
        .child(dialog_title(SharedString::from(format!("{id}.title")), cx).child(title))
        .child(
            dialog_description(SharedString::from(format!("{id}.description")), cx)
                .child(description.into()),
        )
}

/// A confirmation's buttons: Cancel, then the action.
pub struct Confirm<V> {
    id: SharedString,
    action: SharedString,
    cancel: SharedString,
    destructive: bool,
    enabled: bool,
    on_cancel: Handler<V>,
    on_action: Handler<V>,
}

impl<V: 'static> Confirm<V> {
    /// A destructive action named `action`, such as "Discard". The buttons
    /// are `{id}.cancel` and `{id}.confirm`.
    pub fn new(
        id: &str,
        action: impl Into<SharedString>,
        on_cancel: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
        on_action: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
    ) -> Self {
        Self {
            id: id.to_owned().into(),
            action: action.into(),
            cancel: "Cancel".into(),
            destructive: true,
            enabled: true,
            on_cancel: Box::new(on_cancel),
            on_action: Box::new(on_action),
        }
    }

    /// For a dialog that only sometimes destroys something, such as the
    /// branch dialogs that also create.
    #[must_use]
    pub fn destructive(mut self, destructive: bool) -> Self {
        self.destructive = destructive;
        self
    }

    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Names the way out, such as "Back" in a sheet with steps.
    #[must_use]
    pub fn cancel_label(mut self, label: impl Into<SharedString>) -> Self {
        self.cancel = label.into();
        self
    }

    pub fn footer(self, cx: &mut Context<V>) -> Div {
        let Self {
            id,
            action,
            cancel,
            destructive,
            enabled,
            on_cancel,
            on_action,
        } = self;
        dialog_footer(cx)
            .child(
                Button::new(SharedString::from(format!("{id}.cancel")))
                    .variant(ButtonVariant::Outline)
                    .label(cancel)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        on_cancel(this, window, cx);
                    })),
            )
            .child(
                Button::new(SharedString::from(format!("{id}.confirm")))
                    .variant(if destructive {
                        ButtonVariant::Destructive
                    } else {
                        ButtonVariant::Default
                    })
                    .label(action)
                    .disabled(!enabled)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        on_action(this, window, cx);
                    })),
            )
    }
}
