use std::fmt;

use crate::{Error, Result};

/// A Git release version such as `2.39.5`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl GitVersion {
    /// The oldest Git Bonsai supports: `rev-parse --path-format` arrived in 2.31.
    pub const MINIMUM: Self = Self::new(2, 31, 0);

    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parses `git version` output, for example `git version 2.39.5 (Apple Git-154)`.
    pub fn parse(output: &str) -> Result<Self> {
        let rest = output
            .trim()
            .strip_prefix("git version ")
            .ok_or_else(|| Error::Parse(format!("unrecognized version line: {output}")))?;
        let number = rest.split_whitespace().next().unwrap_or_default();
        let mut parts = number.split('.').map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        });
        let mut next = || parts.next().and_then(|p| p.parse::<u32>().ok());
        let major =
            next().ok_or_else(|| Error::Parse(format!("unrecognized version: {number}")))?;
        let minor = next().unwrap_or(0);
        let patch = next().unwrap_or(0);
        Ok(Self::new(major, minor, patch))
    }
}

impl fmt::Display for GitVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_and_vendor_versions() {
        assert_eq!(
            GitVersion::parse("git version 2.39.5 (Apple Git-154)\n").unwrap(),
            GitVersion::new(2, 39, 5)
        );
        assert_eq!(
            GitVersion::parse("git version 2.47.1.windows.2").unwrap(),
            GitVersion::new(2, 47, 1)
        );
        assert_eq!(
            GitVersion::parse("git version 2.50.0-rc1").unwrap(),
            GitVersion::new(2, 50, 0)
        );
        assert!(GitVersion::parse("hello").is_err());
    }

    #[test]
    fn orders_versions() {
        assert!(GitVersion::new(2, 30, 9) < GitVersion::MINIMUM);
        assert!(GitVersion::new(2, 31, 0) >= GitVersion::MINIMUM);
    }
}
