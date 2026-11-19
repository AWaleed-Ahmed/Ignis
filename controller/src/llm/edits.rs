//! Parse the model's answer and apply its line edits to full files.
//!
//! The model only ever sees windows, so it answers with line-range edits inside
//! them. The connector turns those into complete file contents, which is what
//! deploy_revision's patch.files expects. All model output is untrusted.

use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;

use super::window::Window;

pub const MAX_EDITS: usize = 20;
pub const MAX_REPLACEMENT_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid model output: {0}")]
pub struct OutputError(pub String);

fn invalid(message: impl Into<String>) -> OutputError {
    OutputError(message.into())
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AnswerStatus {
    Proposed,
    Declined,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct Edit {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub replacement: String,
}

#[derive(Debug, Deserialize)]
pub struct Answer {
    pub status: AnswerStatus,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub edits: Vec<Edit>,
}

/// Accepts a bare JSON object, optionally wrapped in a markdown code fence.
pub fn parse_answer(text: &str) -> Result<Answer, OutputError> {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|rest| rest.strip_suffix("```"))
        .unwrap_or(trimmed)
        .trim();
    let answer: Answer = serde_json::from_str(body)
        .map_err(|error| invalid(format!("not the expected JSON: {error}")))?;
    if answer.summary.chars().count() > 2000 {
        return Err(invalid("summary is longer than 2000 characters"));
    }
    if answer.status == AnswerStatus::Proposed && answer.edits.is_empty() {
        return Err(invalid("proposed answer has no edits"));
    }
    if answer.status == AnswerStatus::Declined && !answer.edits.is_empty() {
        return Err(invalid("declined answer must not contain edits"));
    }
    Ok(answer)
}

pub fn writable(path: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|prefix| {
        let prefix = prefix.trim_end_matches('/');
        !prefix.is_empty() && (path == prefix || path.starts_with(&format!("{prefix}/")))
    })
}

/// Apply edits to the files they target and return `(path, full content)` pairs.
pub fn apply_edits(
    windows: &[Window],
    writable_prefixes: &[String],
    edits: &[Edit],
) -> Result<Vec<(String, String)>, OutputError> {
    if edits.len() > MAX_EDITS {
        return Err(invalid(format!("more than {MAX_EDITS} edits")));
    }
    let by_path: HashMap<&str, &Window> = windows.iter().map(|w| (w.path.as_str(), w)).collect();
    let mut grouped: BTreeMap<&str, Vec<&Edit>> = BTreeMap::new();
    for edit in edits {
        let window = by_path.get(edit.path.as_str()).ok_or_else(|| {
            invalid(format!(
                "edit targets a file that was not shown: {}",
                edit.path
            ))
        })?;
        if !writable(&edit.path, writable_prefixes) {
            return Err(invalid(format!(
                "edit targets a non-writable path: {}",
                edit.path
            )));
        }
        if edit.replacement.len() > MAX_REPLACEMENT_BYTES {
            return Err(invalid(format!("replacement too large in {}", edit.path)));
        }
        let inside = edit.start_line >= window.start && edit.end_line <= window.end;
        if !inside || edit.start_line > edit.end_line {
            return Err(invalid(format!(
                "edit {}-{} in {} is outside the shown lines {}-{}",
                edit.start_line, edit.end_line, edit.path, window.start, window.end
            )));
        }
        grouped.entry(edit.path.as_str()).or_default().push(edit);
    }
    let mut files = Vec::new();
    for (path, mut file_edits) in grouped {
        let window = by_path[path];
        file_edits.sort_by_key(|edit| edit.start_line);
        if file_edits
            .windows(2)
            .any(|pair| pair[1].start_line <= pair[0].end_line)
        {
            return Err(invalid(format!("overlapping edits in {path}")));
        }
        let mut lines = window.lines.clone();
        for edit in file_edits.iter().rev() {
            let replacement: Vec<String> = edit.replacement.lines().map(str::to_string).collect();
            lines.splice(edit.start_line - 1..edit.end_line, replacement);
        }
        if lines == window.lines {
            return Err(invalid(format!("edits do not change {path}")));
        }
        let mut content = lines.join("\n");
        if window.trailing_newline && !content.is_empty() {
            content.push('\n');
        }
        files.push((path.to_string(), content));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(path: &str, total: usize, start: usize, end: usize) -> Window {
        Window {
            path: path.into(),
            start,
            end,
            lines: (1..=total).map(|n| format!("line {n}")).collect(),
            trailing_newline: true,
        }
    }

    fn edit(path: &str, start: usize, end: usize, replacement: &str) -> Edit {
        Edit {
            path: path.into(),
            start_line: start,
            end_line: end,
            replacement: replacement.into(),
        }
    }

    fn deploy() -> Vec<String> {
        vec!["deploy/".into()]
    }

    #[test]
    fn parses_fenced_and_bare_json() {
        let bare = r#"{"status":"declined","summary":"needs a secret"}"#;
        assert_eq!(parse_answer(bare).unwrap().status, AnswerStatus::Declined);
        let fenced = "```json\n{\"status\":\"proposed\",\"summary\":\"s\",\"edits\":[{\"path\":\"deploy/a.yaml\",\"start_line\":2,\"end_line\":2,\"replacement\":\"x\"}]}\n```";
        assert_eq!(parse_answer(fenced).unwrap().edits.len(), 1);
    }

    #[test]
    fn rejects_malformed_or_contradictory_answers() {
        for text in [
            "not json",
            r#"{"status":"proposed","summary":"s"}"#,
            r#"{"status":"declined","edits":[{"path":"a","start_line":1,"end_line":1,"replacement":""}]}"#,
            r#"{"status":"maybe"}"#,
        ] {
            assert!(parse_answer(text).is_err(), "{text}");
        }
    }

    #[test]
    fn applies_edits_to_the_full_file_bottom_up() {
        let windows = vec![window("deploy/a.yaml", 10, 1, 10)];
        let files = apply_edits(
            &windows,
            &deploy(),
            &[
                edit("deploy/a.yaml", 2, 3, "two\nthree\nextra"),
                edit("deploy/a.yaml", 8, 8, ""),
            ],
        )
        .unwrap();
        assert_eq!(files.len(), 1);
        let lines: Vec<&str> = files[0].1.lines().collect();
        assert_eq!(lines[..5], ["line 1", "two", "three", "extra", "line 4"]);
        assert!(!files[0].1.contains("line 8"));
        assert!(files[0].1.ends_with("line 10\n"));
    }

    #[test]
    fn edits_must_stay_inside_shown_lines_of_shown_writable_files() {
        let windows = vec![
            window("deploy/a.yaml", 300, 100, 220),
            window("src/main.go", 10, 1, 10),
        ];
        let cases = [
            edit("deploy/a.yaml", 99, 100, "x"),
            edit("deploy/a.yaml", 220, 221, "x"),
            edit("deploy/a.yaml", 150, 149, "x"),
            edit("deploy/other.yaml", 1, 1, "x"),
            edit("src/main.go", 1, 1, "x"),
        ];
        for case in cases {
            assert!(
                apply_edits(&windows, &deploy(), &[case.clone()]).is_err(),
                "{case:?}"
            );
        }
        assert!(apply_edits(&windows, &deploy(), &[edit("deploy/a.yaml", 100, 220, "x")]).is_ok());
    }

    #[test]
    fn rejects_overlap_no_op_and_oversized_edits() {
        let windows = vec![window("deploy/a.yaml", 10, 1, 10)];
        let overlap = [
            edit("deploy/a.yaml", 2, 4, "x"),
            edit("deploy/a.yaml", 4, 5, "y"),
        ];
        assert!(apply_edits(&windows, &deploy(), &overlap).is_err());
        assert!(apply_edits(
            &windows,
            &deploy(),
            &[edit("deploy/a.yaml", 2, 2, "line 2")]
        )
        .is_err());
        let huge = "a".repeat(MAX_REPLACEMENT_BYTES + 1);
        assert!(apply_edits(&windows, &deploy(), &[edit("deploy/a.yaml", 2, 2, &huge)]).is_err());
        let many: Vec<Edit> = (1..=21)
            .map(|n| edit("deploy/a.yaml", n % 10 + 1, n % 10 + 1, "z"))
            .collect();
        assert!(apply_edits(&windows, &deploy(), &many).is_err());
    }

    #[test]
    fn writable_prefixes_match_whole_path_segments() {
        let prefixes = vec!["deploy".to_string()];
        assert!(writable("deploy/a.yaml", &prefixes));
        assert!(writable("deploy", &prefixes));
        assert!(!writable("deployments/a.yaml", &prefixes));
        assert!(!writable("x/deploy/a.yaml", &prefixes));
    }
}
