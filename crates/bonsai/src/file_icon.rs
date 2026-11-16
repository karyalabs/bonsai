//! The Lucide icon for a file, from its name, shared by every file list.

use std::path::Path;

use gpui_icons::LucideIcon;

/// Picks an icon by extension or well-known name.
#[must_use]
pub fn file_icon(path: &Path) -> LucideIcon {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if name == ".env" || name.starts_with(".env.") || matches!(extension.as_str(), "pem" | "key") {
        return LucideIcon::FileKey;
    }
    if extension == "lock" || name == "package-lock.json" {
        return LucideIcon::FileLock;
    }
    if matches!(
        name.as_str(),
        "dockerfile" | "makefile" | ".gitignore" | ".gitattributes" | ".editorconfig"
    ) {
        return LucideIcon::FileCog;
    }
    match extension.as_str() {
        "rs" | "swift" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "py" | "go" | "rb" | "java"
        | "kt" | "c" | "h" | "cc" | "cpp" | "hpp" | "m" | "mm" | "cs" | "php" | "scala" | "lua"
        | "zig" | "dart" | "ex" | "exs" | "vue" | "svelte" | "html" | "css" | "scss" | "sql" => {
            LucideIcon::FileCode
        }
        "sh" | "bash" | "zsh" | "fish" | "ps1" => LucideIcon::FileTerminal,
        "json" | "jsonc" => LucideIcon::FileBraces,
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "plist" | "xml" => LucideIcon::FileCog,
        "md" | "mdx" | "txt" | "rst" | "adoc" | "pdf" => LucideIcon::FileText,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "icns" | "bmp" | "tiff"
        | "heic" => LucideIcon::FileImage,
        "zip" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "7z" | "dmg" => LucideIcon::FileArchive,
        "csv" | "tsv" | "xlsx" | "numbers" => LucideIcon::FileSpreadsheet,
        "mp3" | "wav" | "flac" | "aac" | "m4a" => LucideIcon::FileMusic,
        "mp4" | "mov" | "mkv" | "webm" => LucideIcon::FileVideoCamera,
        "ttf" | "otf" | "woff" | "woff2" => LucideIcon::FileType,
        _ => LucideIcon::File,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_icons_by_name_and_extension() {
        assert_eq!(file_icon(Path::new("src/main.rs")), LucideIcon::FileCode);
        assert_eq!(file_icon(Path::new(".env")), LucideIcon::FileKey);
        assert_eq!(file_icon(Path::new("Cargo.lock")), LucideIcon::FileLock);
        assert_eq!(file_icon(Path::new("docs/README.md")), LucideIcon::FileText);
        assert_eq!(file_icon(Path::new("logo.PNG")), LucideIcon::FileImage);
        assert_eq!(file_icon(Path::new("LICENSE")), LucideIcon::File);
    }
}
