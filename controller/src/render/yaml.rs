use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::domain::errors::DomainError;
use crate::domain::models::ManifestSpec;
use crate::render::RenderResult;

pub fn render_yaml(workspace: &str, manifests: &ManifestSpec) -> Result<RenderResult, DomainError> {
    let rel = manifests
        .path
        .as_deref()
        .ok_or_else(|| DomainError::InvalidRequest("manifests.path required for yaml".into()))?;
    let root = PathBuf::from(workspace).join(rel);
    if !root.exists() {
        return Err(DomainError::RenderFailed(format!(
            "yaml path not found: {}",
            root.display()
        )));
    }

    let mut docs = Vec::new();
    if root.is_file() {
        docs.push(read_file(&root)?);
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
        for f in files {
            docs.push(read_file(&f)?);
        }
    }

    if docs.is_empty() {
        return Err(DomainError::RenderFailed("no yaml manifests found".into()));
    }

    Ok(RenderResult {
        yaml: docs.join("\n---\n"),
        render_path: format!("yaml:{}", rel),
    })
}

fn read_file(path: &Path) -> Result<String, DomainError> {
    fs::read_to_string(path)
        .map_err(|e| DomainError::RenderFailed(format!("{}: {e}", path.display())))
}
