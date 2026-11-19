//! Read bounded code windows from the job's own clone, never outside it.

use std::path::{Component, Path, PathBuf};

pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WindowError {
    #[error("path must be repository-relative without '..': {0}")]
    InvalidPath(String),
    #[error("path resolves outside the workspace: {0}")]
    OutsideWorkspace(String),
    #[error("not a regular file: {0}")]
    NotAFile(String),
    #[error("file exceeds {MAX_FILE_BYTES} bytes: {0}")]
    TooLarge(String),
    #[error("file is not UTF-8 text: {0}")]
    NotUtf8(String),
}

/// A file from the clone, plus the 1-based inclusive line range shown to the model.
#[derive(Debug, Clone)]
pub struct Window {
    pub path: String,
    pub start: usize,
    pub end: usize,
    pub lines: Vec<String>,
    pub trailing_newline: bool,
}

impl Window {
    /// Numbered lines so the model can reference exact line ranges.
    pub fn render(&self) -> String {
        (self.start..=self.end)
            .map(|n| format!("{n:>5}| {}", self.lines[n - 1]))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn safe_relative(path: &str) -> Result<PathBuf, WindowError> {
    let candidate = Path::new(path);
    let valid = !path.is_empty()
        && candidate
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if valid {
        Ok(candidate.to_path_buf())
    } else {
        Err(WindowError::InvalidPath(path.to_string()))
    }
}

pub fn read_window(
    root: &Path,
    path: &str,
    line: usize,
    context_lines: usize,
) -> Result<Window, WindowError> {
    let relative = safe_relative(path)?;
    let outside = || WindowError::OutsideWorkspace(path.to_string());
    let root = root.canonicalize().map_err(|_| outside())?;
    // Canonicalising resolves symlinks, so a link inside the repo cannot escape it.
    let full = root.join(relative).canonicalize().map_err(|_| outside())?;
    if !full.starts_with(&root) {
        return Err(outside());
    }
    let metadata = std::fs::metadata(&full).map_err(|_| WindowError::NotAFile(path.to_string()))?;
    if !metadata.is_file() {
        return Err(WindowError::NotAFile(path.to_string()));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(WindowError::TooLarge(path.to_string()));
    }
    let bytes = std::fs::read(&full).map_err(|_| WindowError::NotAFile(path.to_string()))?;
    let text = String::from_utf8(bytes).map_err(|_| WindowError::NotUtf8(path.to_string()))?;
    let trailing_newline = text.ends_with('\n');
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if lines.is_empty() {
        lines.push(String::new());
    }
    let total = lines.len();
    let target = line.clamp(1, total);
    let before = context_lines / 2;
    let start = target.saturating_sub(before).max(1);
    let end = (start + context_lines.max(1) - 1).min(total);
    let start = end.saturating_sub(context_lines.max(1) - 1).max(1);
    Ok(Window {
        path: path.to_string(),
        start,
        end,
        lines,
        trailing_newline,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(lines: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("deploy")).unwrap();
        let body: String = (1..=lines).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.path().join("deploy/app.yaml"), body).unwrap();
        dir
    }

    #[test]
    fn window_is_centred_and_clamped() {
        let dir = repo(500);
        let window = read_window(dir.path(), "deploy/app.yaml", 250, 120).unwrap();
        assert_eq!((window.start, window.end), (190, 309));
        let head = read_window(dir.path(), "deploy/app.yaml", 3, 120).unwrap();
        assert_eq!((head.start, head.end), (1, 120));
        let tail = read_window(dir.path(), "deploy/app.yaml", 499, 120).unwrap();
        assert_eq!((tail.start, tail.end), (381, 500));
        let beyond = read_window(dir.path(), "deploy/app.yaml", 9999, 60).unwrap();
        assert_eq!(beyond.end, 500);
    }

    #[test]
    fn short_files_are_shown_whole_and_rendered_with_numbers() {
        let dir = repo(5);
        let window = read_window(dir.path(), "deploy/app.yaml", 3, 200).unwrap();
        assert_eq!((window.start, window.end), (1, 5));
        assert!(window.trailing_newline);
        assert!(window.render().starts_with("    1| line 1\n    2| line 2"));
    }

    #[test]
    fn traversal_and_absolute_paths_are_rejected() {
        let dir = repo(5);
        for path in [
            "/etc/passwd",
            "../outside",
            "deploy/../../outside",
            "",
            "./deploy/app.yaml",
        ] {
            assert!(
                matches!(
                    read_window(dir.path(), path, 1, 60),
                    Err(WindowError::InvalidPath(_))
                ),
                "{path}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape_the_workspace() {
        let dir = repo(5);
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "token").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("deploy/link.yaml"),
        )
        .unwrap();
        assert_eq!(
            read_window(dir.path(), "deploy/link.yaml", 1, 60).unwrap_err(),
            WindowError::OutsideWorkspace("deploy/link.yaml".into())
        );
    }

    #[test]
    fn directories_missing_large_and_binary_files_are_rejected() {
        let dir = repo(5);
        assert!(matches!(
            read_window(dir.path(), "deploy", 1, 60),
            Err(WindowError::NotAFile(_))
        ));
        assert!(matches!(
            read_window(dir.path(), "deploy/missing.yaml", 1, 60),
            Err(WindowError::OutsideWorkspace(_))
        ));
        std::fs::write(
            dir.path().join("deploy/big.yaml"),
            vec![b'a'; (MAX_FILE_BYTES + 1) as usize],
        )
        .unwrap();
        assert!(matches!(
            read_window(dir.path(), "deploy/big.yaml", 1, 60),
            Err(WindowError::TooLarge(_))
        ));
        std::fs::write(dir.path().join("deploy/bin.yaml"), [0xff, 0xfe, 0x00]).unwrap();
        assert!(matches!(
            read_window(dir.path(), "deploy/bin.yaml", 1, 60),
            Err(WindowError::NotUtf8(_))
        ));
    }
}
