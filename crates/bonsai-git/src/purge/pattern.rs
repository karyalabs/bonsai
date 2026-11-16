//! What a purge removes, and how a path is matched against it.

use std::{ffi::OsString, path::PathBuf};

/// A path or pattern to remove from history (CONTEXT.md: Purge).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PurgeTarget {
    /// A file or folder, relative to the repository root. A folder removes
    /// everything inside it.
    Path(PathBuf),
    /// A glob such as `*.pem` or `config/**/secrets.yml`. `*` and `?` stay
    /// inside one folder and `**` spans folders. A pattern without `/`
    /// matches a file or folder name at any depth.
    Glob(String),
}

impl PurgeTarget {
    /// Whether `path` (repository-relative, `/`-separated bytes) is removed.
    #[must_use]
    pub fn matches(&self, path: &[u8]) -> bool {
        match self {
            Self::Path(target) => {
                let target = normalized(target);
                path.strip_prefix(target.as_slice())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(b"/"))
            }
            Self::Glob(pattern) => {
                let pattern = effective_glob(pattern);
                // The pattern names the path itself or one of its folders.
                std::iter::once(path)
                    .chain(
                        path.iter()
                            .enumerate()
                            .filter(|&(_, &b)| b == b'/')
                            .map(|(i, _)| &path[..i]),
                    )
                    .any(|candidate| wildmatch(pattern.as_bytes(), candidate))
            }
        }
    }

    /// Pathspecs that select the same paths, for path-limited Git walks.
    pub(crate) fn pathspecs(&self) -> Vec<OsString> {
        match self {
            Self::Path(target) => {
                let mut spec = b":(literal)".to_vec();
                spec.extend_from_slice(&normalized(target));
                vec![bytes_to_os(spec)]
            }
            Self::Glob(pattern) => {
                let pattern = effective_glob(pattern);
                vec![
                    OsString::from(format!(":(glob){pattern}")),
                    OsString::from(format!(":(glob){pattern}/**")),
                ]
            }
        }
    }
}

/// The target path as `/`-separated bytes without `./` or a trailing `/`.
fn normalized(path: &std::path::Path) -> Vec<u8> {
    let mut bytes = path.as_os_str().as_encoded_bytes().to_vec();
    if cfg!(windows) {
        for b in &mut bytes {
            if *b == b'\\' {
                *b = b'/';
            }
        }
    }
    let mut start = 0;
    while bytes[start..].starts_with(b"./") {
        start += 2;
    }
    while bytes[start..].starts_with(b"/") {
        start += 1;
    }
    let mut end = bytes.len();
    while end > start && bytes[end - 1] == b'/' {
        end -= 1;
    }
    bytes[start..end].to_vec()
}

fn effective_glob(pattern: &str) -> String {
    let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
    if let Some(anchored) = pattern.strip_prefix('/') {
        anchored.to_owned()
    } else if pattern.contains('/') {
        pattern.to_owned()
    } else {
        format!("**/{pattern}")
    }
}

#[cfg(unix)]
pub(crate) fn bytes_to_os(bytes: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt as _;
    OsString::from_vec(bytes)
}

#[cfg(not(unix))]
pub(crate) fn bytes_to_os(bytes: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&bytes).into_owned())
}

/// Git-style glob matching: `*` and `?` never cross `/`, `**/` matches zero
/// or more folders, a trailing `/**` everything below, `[...]` a class and
/// `\` escapes the next character.
fn wildmatch(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some(b'*') if pattern.get(1) == Some(&b'*') => {
            let rest = &pattern[2..];
            if let Some(after) = rest.strip_prefix(b"/") {
                // `**/`: zero or more whole folders.
                if wildmatch(after, text) {
                    return true;
                }
                text.iter()
                    .enumerate()
                    .filter(|&(_, &b)| b == b'/')
                    .any(|(i, _)| wildmatch(after, &text[i + 1..]))
            } else {
                (0..=text.len()).any(|i| wildmatch(rest, &text[i..]))
            }
        }
        Some(b'*') => {
            let rest = &pattern[1..];
            for i in 0..=text.len() {
                if wildmatch(rest, &text[i..]) {
                    return true;
                }
                if text.get(i) == Some(&b'/') {
                    break;
                }
            }
            false
        }
        Some(b'?') => text
            .first()
            .is_some_and(|&c| c != b'/' && wildmatch(&pattern[1..], &text[1..])),
        Some(b'[') => {
            let Some(&c) = text.first() else {
                return false;
            };
            match class(&pattern[1..], c) {
                Some((matched, rest)) => c != b'/' && matched && wildmatch(rest, &text[1..]),
                // An unclosed `[` is a literal.
                None => c == b'[' && wildmatch(&pattern[1..], &text[1..]),
            }
        }
        Some(b'\\') if pattern.len() > 1 => {
            text.first() == Some(&pattern[1]) && wildmatch(&pattern[2..], &text[1..])
        }
        Some(&p) => text.first() == Some(&p) && wildmatch(&pattern[1..], &text[1..]),
    }
}

/// Matches `c` against a class body (after `[`); returns whether it matched
/// and the pattern after the closing `]`.
fn class(body: &[u8], c: u8) -> Option<(bool, &[u8])> {
    let (negated, mut i) = match body.first() {
        Some(b'!' | b'^') => (true, 1),
        _ => (false, 0),
    };
    let mut matched = false;
    let mut first = true;
    while i < body.len() {
        let mut low = body[i];
        if low == b']' && !first {
            return Some((matched != negated, &body[i + 1..]));
        }
        first = false;
        if low == b'\\' && i + 1 < body.len() {
            i += 1;
            low = body[i];
        }
        if body.get(i + 1) == Some(&b'-') && body.get(i + 2).is_some_and(|&b| b != b']') {
            let high = body[i + 2];
            matched |= (low..=high).contains(&c);
            i += 3;
        } else {
            matched |= low == c;
            i += 1;
        }
    }
    None
}

/// Decodes a path as `git fast-export` writes it: verbatim, or in double
/// quotes with C-style escapes when it holds special bytes.
pub(crate) fn unquote(field: &[u8]) -> Option<Vec<u8>> {
    let Some(inner) = field.strip_prefix(b"\"") else {
        return Some(field.to_vec());
    };
    let inner = inner.strip_suffix(b"\"")?;
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        let b = inner[i];
        if b != b'\\' {
            out.push(b);
            i += 1;
            continue;
        }
        let escaped = *inner.get(i + 1)?;
        i += 2;
        out.push(match escaped {
            b'a' => 0x07,
            b'b' => 0x08,
            b'f' => 0x0c,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 0x0b,
            b'0'..=b'3' => {
                let digits = inner.get(i..i + 2)?;
                if !digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                    return None;
                }
                i += 2;
                (escaped - b'0') * 64 + (digits[0] - b'0') * 8 + (digits[1] - b'0')
            }
            other => other,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glob(pattern: &str, path: &str) -> bool {
        PurgeTarget::Glob(pattern.into()).matches(path.as_bytes())
    }

    #[test]
    fn paths_match_themselves_and_their_contents() {
        let target = PurgeTarget::Path("config/.env".into());
        assert!(target.matches(b"config/.env"));
        assert!(target.matches(b"config/.env/old"));
        assert!(!target.matches(b"config/.env.example"));
        assert!(!target.matches(b".env"));
        assert!(PurgeTarget::Path("./secrets/".into()).matches(b"secrets/a/b.key"));
    }

    #[test]
    fn globs_follow_git_rules() {
        assert!(glob("*.pem", "key.pem"));
        assert!(glob("*.pem", "deep/down/key.pem"));
        assert!(!glob("/*.pem", "deep/key.pem"));
        assert!(glob("config/*.yml", "config/a.yml"));
        assert!(!glob("config/*.yml", "config/sub/a.yml"));
        assert!(glob("config/**/a.yml", "config/a.yml"));
        assert!(glob("config/**/a.yml", "config/x/y/a.yml"));
        assert!(glob("secrets", "app/secrets/token"));
        assert!(glob(".env*", ".env.local"));
        assert!(glob("key[0-9].txt", "key7.txt"));
        assert!(!glob("key[!0-9].txt", "key7.txt"));
        assert!(glob("a\\*b", "a*b"));
        assert!(!glob("a\\*b", "axb"));
        assert!(glob("?.txt", "x.txt"));
        assert!(!glob("node_modules", "node_modules_backup/x"));
    }

    #[test]
    fn quoted_paths_decode() {
        assert_eq!(unquote(b"plain name").unwrap(), b"plain name");
        assert_eq!(
            unquote(b"\"sp ace \\303\\251.txt\"").unwrap(),
            "sp ace é.txt".as_bytes()
        );
        assert_eq!(unquote(b"\"a\\\"b\\\\c\\n\"").unwrap(), b"a\"b\\c\n");
        assert_eq!(unquote(b"\"\\377\"").unwrap(), [0xff]);
        assert!(unquote(b"\"open").is_none());
    }
}
