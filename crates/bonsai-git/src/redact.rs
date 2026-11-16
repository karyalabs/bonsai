//! Keeps credentials out of the activity log.

/// Replaces the userinfo of any `scheme://user:secret@host` URL with `***`.
pub(crate) fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(scheme_end) = rest.find("://") {
        let (head, tail) = rest.split_at(scheme_end + 3);
        out.push_str(head);
        let authority_end = tail
            .find(|c: char| c == '/' || c.is_whitespace() || c == '\'' || c == '"')
            .unwrap_or(tail.len());
        let authority = &tail[..authority_end];
        if let Some(at) = authority.rfind('@') {
            out.push_str("***");
            out.push_str(&authority[at..]);
        } else {
            out.push_str(authority);
        }
        rest = &tail[authority_end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn hides_url_credentials() {
        assert_eq!(
            redact("push https://x-access-token:ghp_abc@github.com/a/b.git main"),
            "push https://***@github.com/a/b.git main"
        );
        assert_eq!(
            redact("fatal: 'https://tok@gitlab.com/x' not found"),
            "fatal: 'https://***@gitlab.com/x' not found"
        );
    }

    #[test]
    fn leaves_other_text_alone() {
        let text = "fetch git@github.com:a/b.git and https://github.com/a/b";
        assert_eq!(redact(text), text);
    }
}
