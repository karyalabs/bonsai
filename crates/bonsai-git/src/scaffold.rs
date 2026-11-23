//! Starting files for a new repository: a `.gitignore` for the project's
//! language, a README and a license (CONTEXT.md: New repository).

use std::path::{Path, PathBuf};

use crate::{Error, Git, Repository, Result};

/// A starting `.gitignore`. Every template also ignores macOS's `.DS_Store`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IgnoreTemplate {
    #[default]
    None,
    MacOs,
    Rust,
    Node,
    Python,
    Swift,
    Go,
    Java,
}

impl IgnoreTemplate {
    pub const ALL: [Self; 8] = [
        Self::None,
        Self::MacOs,
        Self::Rust,
        Self::Node,
        Self::Python,
        Self::Swift,
        Self::Go,
        Self::Java,
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::MacOs => "macOS only",
            Self::Rust => "Rust",
            Self::Node => "Node",
            Self::Python => "Python",
            Self::Swift => "Swift and Xcode",
            Self::Go => "Go",
            Self::Java => "Java and Kotlin",
        }
    }

    /// The file's text, or `None` for no file.
    #[must_use]
    pub fn contents(self) -> Option<String> {
        let language = match self {
            Self::None => return None,
            Self::MacOs => "",
            Self::Rust => "# Rust\n/target/\n**/*.rs.bk\n",
            Self::Node => {
                "# Node\nnode_modules/\ndist/\nbuild/\ncoverage/\n.env\n.env.*\n!.env.example\nnpm-debug.log*\nyarn-error.log*\n"
            }
            Self::Python => {
                "# Python\n__pycache__/\n*.py[cod]\n.venv/\nvenv/\nbuild/\ndist/\n*.egg-info/\n.pytest_cache/\n.mypy_cache/\n.env\n"
            }
            Self::Swift => {
                "# Swift and Xcode\n.build/\nDerivedData/\n*.xcuserstate\nxcuserdata/\n*.xcscmblueprint\n.swiftpm/\nPods/\n"
            }
            Self::Go => "# Go\n/bin/\n*.exe\n*.test\n*.out\ncoverage.*\n",
            Self::Java => {
                "# Java and Kotlin\n*.class\n*.jar\ntarget/\nbuild/\n.gradle/\nout/\n.idea/\n*.iml\n"
            }
        };
        let mac = "# macOS\n.DS_Store\n";
        Some(if language.is_empty() {
            mac.to_owned()
        } else {
            format!("{language}\n{mac}")
        })
    }
}

/// A starting license.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum License {
    #[default]
    None,
    Mit,
    Isc,
}

impl License {
    pub const ALL: [Self; 3] = [Self::None, Self::Mit, Self::Isc];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Mit => "MIT",
            Self::Isc => "ISC",
        }
    }

    /// The license text for `holder` in `year`, or `None` for no file.
    #[must_use]
    pub fn text(self, year: i32, holder: &str) -> Option<String> {
        match self {
            Self::None => None,
            Self::Mit => Some(format!(
                "MIT License\n\nCopyright (c) {year} {holder}\n\n\
Permission is hereby granted, free of charge, to any person obtaining a copy\n\
of this software and associated documentation files (the \"Software\"), to deal\n\
in the Software without restriction, including without limitation the rights\n\
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell\n\
copies of the Software, and to permit persons to whom the Software is\n\
furnished to do so, subject to the following conditions:\n\n\
The above copyright notice and this permission notice shall be included in all\n\
copies or substantial portions of the Software.\n\n\
THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR\n\
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,\n\
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE\n\
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER\n\
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,\n\
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE\n\
SOFTWARE.\n"
            )),
            Self::Isc => Some(format!(
                "ISC License\n\nCopyright (c) {year} {holder}\n\n\
Permission to use, copy, modify, and/or distribute this software for any\n\
purpose with or without fee is hereby granted, provided that the above\n\
copyright notice and this permission notice appear in all copies.\n\n\
THE SOFTWARE IS PROVIDED \"AS IS\" AND THE AUTHOR DISCLAIMS ALL WARRANTIES\n\
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF\n\
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR\n\
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES\n\
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN\n\
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF\n\
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.\n"
            )),
        }
    }
}

/// How to start a new repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRepository {
    pub path: PathBuf,
    /// The first branch, such as `main`; `None` uses Git's default.
    pub branch: Option<String>,
    pub ignore: IgnoreTemplate,
    pub readme: bool,
    pub license: License,
    /// The license's copyright holder.
    pub holder: String,
    pub year: i32,
}

impl NewRepository {
    /// The files to write, as `(name, contents)`.
    #[must_use]
    pub fn files(&self) -> Vec<(&'static str, String)> {
        let mut files = Vec::new();
        if let Some(text) = self.ignore.contents() {
            files.push((".gitignore", text));
        }
        if self.readme {
            let title = self.path.file_name().map_or_else(
                || "Project".to_owned(),
                |n| n.to_string_lossy().into_owned(),
            );
            files.push(("README.md", format!("# {title}\n")));
        }
        if let Some(text) = self.license.text(self.year, self.holder.trim()) {
            files.push(("LICENSE", text));
        }
        files
    }
}

/// Whether `path` can hold a new repository: missing, or an empty folder.
#[must_use]
pub fn is_empty_or_missing(path: &Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

impl Repository {
    /// Creates the folder, starts a repository in it and writes the chosen
    /// files, staged and ready for a first commit. Refuses a folder that
    /// already has files.
    pub fn create(git: &Git, new: &NewRepository) -> Result<Self> {
        if !is_empty_or_missing(&new.path) {
            return Err(Error::Refused(format!(
                "{} already has files in it. Choose an empty or new folder.",
                new.path.display()
            )));
        }
        std::fs::create_dir_all(&new.path).map_err(|source| Error::Io {
            path: new.path.clone(),
            source,
        })?;
        let branch = new
            .branch
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty());
        let repository = Self::init(git, &new.path, branch)?;
        let files = new.files();
        for (name, contents) in &files {
            let path = new.path.join(name);
            std::fs::write(&path, contents).map_err(|source| Error::Io { path, source })?;
        }
        if !files.is_empty() {
            repository.stage_all()?;
        }
        Ok(repository)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_end_with_the_macos_rules() {
        assert_eq!(IgnoreTemplate::None.contents(), None);
        for template in &IgnoreTemplate::ALL[1..] {
            let text = template.contents().unwrap();
            assert!(text.ends_with(".DS_Store\n"), "{template:?}");
        }
        assert!(
            IgnoreTemplate::Rust
                .contents()
                .unwrap()
                .starts_with("# Rust\n/target/")
        );
    }

    #[test]
    fn licenses_name_the_holder_and_year() {
        assert_eq!(License::None.text(2026, "Ada"), None);
        let mit = License::Mit.text(2026, "Ada Lovelace").unwrap();
        assert!(mit.starts_with("MIT License\n\nCopyright (c) 2026 Ada Lovelace\n"));
        assert!(
            License::Isc
                .text(2026, "Ada")
                .unwrap()
                .contains("ISC License")
        );
    }
}
