//! Capture client tool versions for deploy/validate artifacts (P1).

use std::collections::BTreeMap;
use std::process::Command;

/// Best-effort versions of helm/kubectl/kustomize on PATH.
pub fn collect_tool_versions() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(v) = tool_version_line("kubectl", &["version", "--client", "--short"]) {
        out.insert("kubectl".into(), v);
    } else if let Some(v) = tool_version_line("kubectl", &["version", "--client"]) {
        out.insert("kubectl".into(), first_line(&v));
    }
    if let Some(v) = tool_version_line("helm", &["version", "--short"]) {
        out.insert("helm".into(), v);
    } else if let Some(v) = tool_version_line("helm", &["version"]) {
        out.insert("helm".into(), first_line(&v));
    }
    if let Some(v) = tool_version_line("kustomize", &["version"]) {
        out.insert("kustomize".into(), first_line(&v));
    }
    out
}

fn tool_version_line(bin: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(bin).args(args).output().ok()?;
    if !output.status.success() && output.stdout.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    let combined = if text.trim().is_empty() {
        err.to_string()
    } else {
        text.to_string()
    };
    let line = first_line(&combined);
    if line.is_empty() {
        None
    } else {
        Some(line)
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}
