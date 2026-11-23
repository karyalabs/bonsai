//! Finding what is worth purging: files with secret-looking names, contents
//! that look like credentials, and large files, anywhere in history
//! (CONTEXT.md: Purge).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
};

use super::bytes_to_path;
use crate::{Repository, Result};

/// What a history scan looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanOptions {
    /// A file version at least this big is reported as large.
    pub large_bytes: u64,
    /// Whether to read file contents for credentials.
    pub contents: bool,
    /// Versions bigger than this are not read for credentials.
    pub content_max_blob: u64,
    /// Reading stops after this many bytes, and the scan says so.
    pub content_budget: u64,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            large_bytes: 5 * 1024 * 1024,
            contents: true,
            content_max_blob: 1024 * 1024,
            content_budget: 512 * 1024 * 1024,
        }
    }
}

/// Why a path is worth purging.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScanReason {
    /// Some version contains something that looks like this kind of
    /// credential, such as "Private key".
    Credential(&'static str),
    /// The file name is one that usually holds secrets, such as `.env`.
    SensitiveName,
    /// Some version is this many bytes.
    Large(u64),
}

impl ScanReason {
    /// A short phrase for the purge sheet.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Credential(kind) => format!("{kind} found"),
            Self::SensitiveName => "Usually holds secrets".to_owned(),
            Self::Large(bytes) => format!("Large file, {}", format_bytes(*bytes)),
        }
    }
}

/// One path a scan suggests purging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanFinding {
    pub path: PathBuf,
    /// Most serious first.
    pub reasons: Vec<ScanReason>,
    /// The biggest version of the file in history.
    pub largest: u64,
    /// Whether the current commit still has the file.
    pub in_head: bool,
}

impl ScanFinding {
    /// Whether the finding is about secrets rather than size.
    #[must_use]
    pub fn is_secret(&self) -> bool {
        self.reasons
            .iter()
            .any(|r| matches!(r, ScanReason::Credential(_) | ScanReason::SensitiveName))
    }
}

/// What a scan found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryScan {
    /// Secrets first, then large files by size.
    pub findings: Vec<ScanFinding>,
    /// File versions read for credentials.
    pub blobs_read: usize,
    /// Whether every version under the size limit was read. `false` when the
    /// budget ran out.
    pub contents_complete: bool,
}

/// Formats a byte count as `4.2 MB`.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

const EXAMPLES: [&str; 6] = [
    "example", "sample", "template", "dist", "defaults", "schema",
];
const NAMES: [&str; 12] = [
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    ".netrc",
    ".pypirc",
    ".htpasswd",
    ".git-credentials",
    "credentials.json",
    "secrets.yml",
    "secrets.yaml",
    "secrets.json",
];
const EXTENSIONS: [&str; 8] = [
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".keystore",
    ".jks",
    ".tfstate",
    ".ppk",
];

/// Whether a file name usually holds secrets: `.env` files (not examples),
/// private keys, keystores, credential files and Terraform state.
// The name is lowercased first, so the extension checks ignore case.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
#[must_use]
pub fn sensitive_name(path: &[u8]) -> bool {
    let name = path.rsplit(|&b| b == b'/').next().unwrap_or(path);
    let name = String::from_utf8_lossy(name).to_ascii_lowercase();
    if name == ".env" || name.starts_with(".env.") {
        return !EXAMPLES.iter().any(|e| name.contains(e));
    }
    if name.ends_with(".env") && !EXAMPLES.iter().any(|e| name.contains(e)) {
        return true;
    }
    NAMES.contains(&name.as_str())
        || EXTENSIONS.iter().any(|e| name.ends_with(e))
        || (name.starts_with("service-account") && name.ends_with(".json"))
}

/// A credential pattern: a literal prefix and the run of characters that
/// must follow it.
struct Pattern {
    kind: &'static str,
    prefix: &'static [u8],
    min_tail: usize,
    tail: fn(u8) -> bool,
}

fn alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn upper_alnum(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit()
}

fn token(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

const PATTERNS: &[Pattern] = &[
    Pattern {
        kind: "AWS access key",
        prefix: b"AKIA",
        min_tail: 16,
        tail: upper_alnum,
    },
    Pattern {
        kind: "GitHub token",
        prefix: b"ghp_",
        min_tail: 36,
        tail: alnum,
    },
    Pattern {
        kind: "GitHub token",
        prefix: b"gho_",
        min_tail: 36,
        tail: alnum,
    },
    Pattern {
        kind: "GitHub token",
        prefix: b"ghu_",
        min_tail: 36,
        tail: alnum,
    },
    Pattern {
        kind: "GitHub token",
        prefix: b"ghs_",
        min_tail: 36,
        tail: alnum,
    },
    Pattern {
        kind: "GitHub token",
        prefix: b"github_pat_",
        min_tail: 40,
        tail: token,
    },
    Pattern {
        kind: "GitLab token",
        prefix: b"glpat-",
        min_tail: 20,
        tail: token,
    },
    Pattern {
        kind: "Slack token",
        prefix: b"xoxb-",
        min_tail: 20,
        tail: token,
    },
    Pattern {
        kind: "Slack token",
        prefix: b"xoxp-",
        min_tail: 20,
        tail: token,
    },
    Pattern {
        kind: "Google API key",
        prefix: b"AIza",
        min_tail: 35,
        tail: token,
    },
    Pattern {
        kind: "Stripe secret key",
        prefix: b"sk_live_",
        min_tail: 24,
        tail: alnum,
    },
];

/// The kinds of credential `text` seems to contain, in [`PATTERNS`] order.
#[must_use]
pub fn credentials_in(text: &[u8]) -> Vec<&'static str> {
    let mut kinds = Vec::new();
    if contains(text, b"-----BEGIN ") && find_private_key(text) {
        kinds.push("Private key");
    }
    for pattern in PATTERNS {
        if kinds.contains(&pattern.kind) {
            continue;
        }
        let mut at = 0;
        while let Some(found) = find(&text[at..], pattern.prefix) {
            let start = at + found;
            let before_ok = start == 0 || !token(text[start - 1]);
            let tail = &text[start + pattern.prefix.len()..];
            let run = tail.iter().take_while(|&&b| (pattern.tail)(b)).count();
            if before_ok && run >= pattern.min_tail {
                kinds.push(pattern.kind);
                break;
            }
            at = start + 1;
        }
    }
    kinds
}

fn find_private_key(text: &[u8]) -> bool {
    let mut at = 0;
    while let Some(found) = find(&text[at..], b"-----BEGIN ") {
        let start = at + found + 11;
        let line = text[start..]
            .split(|&b| b == b'\n')
            .next()
            .unwrap_or_default();
        if contains(line, b"PRIVATE KEY-----") {
            return true;
        }
        at = start;
    }
    false
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let first = *needle.first()?;
    let mut at = 0;
    while at + needle.len() <= haystack.len() {
        let offset = haystack[at..].iter().position(|&b| b == first)?;
        at += offset;
        if haystack[at..].starts_with(needle) {
            return Some(at);
        }
        at += 1;
    }
    None
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    find(haystack, needle).is_some()
}

/// Reads `cat-file --batch` output: each object's id and contents.
fn batch_objects(output: &[u8]) -> Vec<(&[u8], &[u8])> {
    let mut objects = Vec::new();
    let mut rest = output;
    while let Some(newline) = rest.iter().position(|&b| b == b'\n') {
        let header = &rest[..newline];
        let mut fields = header.split(|&b| b == b' ');
        let (Some(oid), Some(_kind), Some(size)) = (fields.next(), fields.next(), fields.next())
        else {
            break;
        };
        let Some(size) = std::str::from_utf8(size)
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        else {
            break;
        };
        let body_start = newline + 1;
        let body_end = (body_start + size).min(rest.len());
        objects.push((oid, &rest[body_start..body_end]));
        rest = rest.get(body_end + 1..).unwrap_or_default();
    }
    objects
}

impl Repository {
    /// Looks through every commit on every branch and tag for files worth
    /// purging. Stashes are left out, like a purge leaves them alone.
    pub fn scan_history(&self, options: &ScanOptions) -> Result<HistoryScan> {
        let mut reasons: BTreeMap<PathBuf, BTreeSet<ScanReason>> = BTreeMap::new();
        let mut largest: HashMap<PathBuf, u64> = HashMap::new();

        for path in self.history_paths()? {
            if sensitive_name(path.to_string_lossy().as_bytes()) {
                reasons
                    .entry(path)
                    .or_default()
                    .insert(ScanReason::SensitiveName);
            }
        }
        let has_commits = !self.refs()?.is_empty();
        if !has_commits {
            return Ok(HistoryScan {
                contents_complete: true,
                ..HistoryScan::default()
            });
        }

        // Every blob with the first path Git found it at.
        let objects = self.git_out(["rev-list", "--objects", "--exclude=refs/stash", "--all"])?;
        let mut paths: HashMap<&[u8], &[u8]> = HashMap::new();
        for line in objects.split(|&b| b == b'\n') {
            if let Some(space) = line.iter().position(|&b| b == b' ') {
                paths.entry(&line[..space]).or_insert(&line[space + 1..]);
            }
        }
        let mut input = Vec::new();
        for oid in paths.keys() {
            input.extend_from_slice(oid);
            input.push(b'\n');
        }
        let checked = self.git().run(
            &self
                .git()
                .command([
                    "cat-file",
                    "--batch-check=%(objectname) %(objecttype) %(objectsize)",
                ])
                .cwd(self.root())
                .stdin(input),
        )?;
        let mut blobs: Vec<(&[u8], u64)> = Vec::new();
        for line in checked.stdout.split(|&b| b == b'\n') {
            let mut fields = line.split(|&b| b == b' ');
            let (Some(oid), Some(b"blob"), Some(size)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Some(size) = std::str::from_utf8(size).ok().and_then(|s| s.parse().ok()) else {
                continue;
            };
            let Some(&path) = paths.get(oid) else {
                continue;
            };
            let path = bytes_to_path(path);
            let biggest = largest.entry(path.clone()).or_default();
            *biggest = (*biggest).max(size);
            if size >= options.large_bytes {
                reasons
                    .entry(path)
                    .or_default()
                    .insert(ScanReason::Large(0));
            }
            blobs.push((oid, size));
        }

        let mut blobs_read = 0;
        let mut contents_complete = true;
        if options.contents {
            let mut candidates: Vec<&[u8]> = blobs
                .iter()
                .filter(|(_, size)| *size > 0 && *size <= options.content_max_blob)
                .map(|(oid, _)| *oid)
                .collect();
            candidates.sort_unstable();
            let mut spent = 0u64;
            let mut chunk: Vec<u8> = Vec::new();
            let mut chunk_bytes = 0u64;
            let sizes: HashMap<&[u8], u64> = blobs.iter().copied().collect();
            let mut pending: Vec<Vec<u8>> = Vec::new();
            for oid in candidates {
                let size = sizes[oid];
                if spent + size > options.content_budget {
                    contents_complete = false;
                    break;
                }
                spent += size;
                chunk.extend_from_slice(oid);
                chunk.push(b'\n');
                chunk_bytes += size;
                if chunk_bytes >= 32 * 1024 * 1024 {
                    pending.push(std::mem::take(&mut chunk));
                    chunk_bytes = 0;
                }
            }
            if !chunk.is_empty() {
                pending.push(chunk);
            }
            for input in pending {
                let output = self.git().run(
                    &self
                        .git()
                        .command(["cat-file", "--batch"])
                        .cwd(self.root())
                        .stdin(input),
                )?;
                for (oid, body) in batch_objects(&output.stdout) {
                    blobs_read += 1;
                    let sample = &body[..body.len().min(8000)];
                    if sample.contains(&0) {
                        continue;
                    }
                    let kinds = credentials_in(body);
                    if kinds.is_empty() {
                        continue;
                    }
                    let Some(&path) = paths.get(oid) else {
                        continue;
                    };
                    let entry = reasons.entry(bytes_to_path(path)).or_default();
                    entry.extend(kinds.into_iter().map(ScanReason::Credential));
                }
            }
        }

        let head = self.git().run_unchecked(
            &self
                .git()
                .command(["ls-tree", "-r", "-z", "--name-only", "HEAD"])
                .cwd(self.root()),
        )?;
        let in_head: BTreeSet<PathBuf> = head
            .stdout
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(bytes_to_path)
            .collect();

        let mut findings: Vec<ScanFinding> = reasons
            .into_iter()
            .map(|(path, reasons)| {
                let size = largest.get(&path).copied().unwrap_or_default();
                let reasons = reasons
                    .into_iter()
                    .map(|r| match r {
                        ScanReason::Large(_) => ScanReason::Large(size),
                        other => other,
                    })
                    .collect();
                ScanFinding {
                    in_head: in_head.contains(&path),
                    path,
                    reasons,
                    largest: size,
                }
            })
            .collect();
        findings.sort_by(|a, b| {
            b.is_secret()
                .cmp(&a.is_secret())
                .then(b.largest.cmp(&a.largest))
                .then(a.path.cmp(&b.path))
        });
        Ok(HistoryScan {
            findings,
            blobs_read,
            contents_complete,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_secret_file_names() {
        for name in [
            ".env",
            "app/.env.production",
            "config/prod.env",
            "keys/server.pem",
            "deploy/id_ed25519",
            "service-account-prod.json",
            "infra/terraform.tfstate",
        ] {
            assert!(sensitive_name(name.as_bytes()), "{name}");
        }
        for name in [
            ".env.example",
            ".env.sample",
            "README.md",
            "src/environment.rs",
            "id_rsa.pub",
            "keys.rs",
        ] {
            assert!(!sensitive_name(name.as_bytes()), "{name}");
        }
    }

    #[test]
    fn finds_credentials_in_text() {
        let key = format!("AWS_KEY=AKIA{}\n", "ABCDEFGHIJKLMNOP");
        assert_eq!(credentials_in(key.as_bytes()), ["AWS access key"]);
        let pem = b"-----BEGIN RSA PRIVATE KEY-----\nMIIE...\n";
        assert_eq!(credentials_in(pem), ["Private key"]);
        let github = format!("token: ghp_{}", "a".repeat(36));
        assert_eq!(credentials_in(github.as_bytes()), ["GitHub token"]);
        assert_eq!(
            credentials_in(b"-----BEGIN PUBLIC KEY-----"),
            Vec::<&str>::new()
        );
        assert_eq!(
            credentials_in(b"let ghp_short = 1; // AKIA"),
            Vec::<&str>::new()
        );
        let embedded = format!("xAKIA{}", "ABCDEFGHIJKLMNOP");
        assert_eq!(credentials_in(embedded.as_bytes()), Vec::<&str>::new());
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(4_200_000), "4.2 MB");
        assert_eq!(format_bytes(250_000_000), "250 MB");
    }

    #[test]
    fn reads_batch_output() {
        let output = b"aaa blob 3\nabc\nbbb blob 0\n\nccc blob 2\nhi\n";
        let objects = batch_objects(output);
        assert_eq!(
            objects,
            [(&b"aaa"[..], &b"abc"[..]), (b"bbb", b""), (b"ccc", b"hi")]
        );
    }
}
