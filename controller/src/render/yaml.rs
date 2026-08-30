use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::domain::errors::DomainError;
use crate::domain::models::{ManifestSpec, RenderedFile};
use crate::render::RenderResult;

pub fn render_yaml(workspace: &str, manifests: &ManifestSpec) -> Result<RenderResult, DomainError> {
    let rel = manifests
        .path
        .as_deref()
        .ok_or_else(|| DomainError::InvalidRequest("manifests.path required for yaml".into()))?;
    let workspace_root = PathBuf::from(workspace);
    let root = workspace_root.join(rel);
    if !root.exists() {
        return Err(DomainError::RenderFailed(format!(
            "yaml path not found: {}",
            root.display()
        )));
    }

    let mut sources: Vec<PathBuf> = Vec::new();
    if root.is_file() {
        sources.push(root.clone());
    } else {
        let mut files: Vec<PathBuf> = WalkDir::new(&root)
            .into_iter()
            .filter_map(|e| e.ok())
            .map(|e| e.into_path())
            .filter(|p| {
                p.extension()
                    .and_then(|x| x.to_str())
                    .map(|ext| ext == "yaml" || ext == "yml")
                    .unwrap_or(false)
            })
            .collect();
        files.sort();
        sources = files;
    }

    if sources.is_empty() {
        return Err(DomainError::RenderFailed("no yaml manifests found".into()));
    }

    let mut docs = Vec::new();
    let mut files = Vec::new();
    for path in &sources {
        let content = read_file(path)?;
        let rel_path = path
            .strip_prefix(&workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        files.push(RenderedFile {
            path: rel_path,
            content: content.clone(),
        });
        docs.push(content);
    }

    Ok(RenderResult {
        yaml: docs.join("\n---\n"),
        render_path: format!("yaml:{}", rel),
        files,
    })
}

fn read_file(path: &Path) -> Result<String, DomainError> {
    fs::read_to_string(path)
        .map_err(|e| DomainError::RenderFailed(format!("{}: {e}", path.display())))
}
