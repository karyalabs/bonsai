//! A person's initials in a colored circle, the same color for the same
//! email everywhere, for History rows and commit details.

use gpui_kit::{Div, Hsla, ParentElement as _, Pixels, Styled, div, hsla};

/// One or two letters for `name`: first and last word's initials.
#[must_use]
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let first = |word: &str| word.chars().next().map(|c| c.to_uppercase().to_string());
    match words.as_slice() {
        [] => "?".to_owned(),
        [one] => first(one).unwrap_or_default(),
        [one, .., last] => first(one).unwrap_or_default() + &first(last).unwrap_or_default(),
    }
}

/// A stable hue for `key`, from its FNV-1a hash.
#[must_use]
pub fn hue(key: &str) -> f32 {
    let hash = key.to_lowercase().bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    // 24 evenly spaced hues keep neighbors apart.
    f32::from(u16::try_from(hash % 24).unwrap_or(0)) / 24.
}

/// The circle, `size` across, colored by `email` (or the name without one).
#[must_use]
pub fn avatar(name: &str, email: &str, size: Pixels, dark: bool) -> Div {
    let key = if email.is_empty() { name } else { email };
    let hue = hue(key);
    let (background, foreground): (Hsla, Hsla) = if dark {
        (hsla(hue, 0.45, 0.32, 1.), hsla(hue, 0.6, 0.88, 1.))
    } else {
        (hsla(hue, 0.6, 0.88, 1.), hsla(hue, 0.55, 0.3, 1.))
    };
    div()
        .flex_shrink_0()
        .size(size)
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(background)
        .text_color(foreground)
        .text_size(size * 0.42)
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .child(initials(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_and_hues_are_stable() {
        assert_eq!(initials("Ada Lovelace"), "AL");
        assert_eq!(initials("Grace Brewster Hopper"), "GH");
        assert_eq!(initials("linus"), "L");
        assert_eq!(initials("  "), "?");
        assert_eq!(hue("ada@example.com"), hue("ADA@example.com"));
        assert!((0. ..1.).contains(&hue("someone")));
    }
}
