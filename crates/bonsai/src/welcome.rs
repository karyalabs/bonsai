//! The first screen: no repository is open yet.

use gpui_kit::{Context, IntoElement, ParentElement as _, Render, Styled, Window, div, px};

use crate::ui::theme::UiTheme;

pub struct Welcome;

impl Render for Welcome {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .bg(theme.colors.background)
            .text_color(theme.colors.foreground)
            .font_family(theme.fonts.body.clone())
            .child(div().text_xl().child("Bonsai"))
            .child(
                div()
                    .text_sm()
                    .text_color(theme.colors.muted_foreground)
                    .child("No repository open"),
            )
    }
}
