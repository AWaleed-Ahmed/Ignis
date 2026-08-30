use std::path::PathBuf;
use std::process::Stdio;

use crate::domain::errors::DomainError;
use crate::domain::models::ManifestSpec;
use crate::render::RenderResult;

pub fn render_kustomize(
    workspace: &str,
    manifests: &ManifestSpec,
) -> Result<RenderResult, DomainError> {
    let overlay = manifests
        .overlay
        .as_deref()
        .or(manifests.path.as_deref())
        .ok_or_else(|| {
            DomainError::InvalidRequest("manifests.overlay (or path) required for kustomize".into())
        })?;
    let root = PathBuf::from(workspace).join(overlay);
    if !root.exists() {
        return Err(DomainError::RenderFailed(format!(
            "kustomize overlay not found: {}",
            root.display()
        )));
    }

    // Prefer kubectl kustomize; fall back to reading resources listed in kustomization if tools missing.
    let output = std::process::Command::new("kubectl")
        .args(["kustomize", &root.to_string_lossy()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(out) if out.status.success() => {
            return Ok(RenderResult {
                yaml: String::from_utf8_lossy(&out.stdout).to_string(),
                render_path: format!("kustomize:{overlay}"),
                files: Vec::new(),
            });
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            // try kustomize binary
            let alt = std::process::Command::new("kustomize")
                .args(["build", &root.to_string_lossy()])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output();
            if let Ok(out2) = alt {
                if out2.status.success() {
                    return Ok(RenderResult {
                        yaml: String::from_utf8_lossy(&out2.stdout).to_string(),
                        render_path: format!("kustomize:{overlay}"),
                        files: Vec::new(),
                    });
                }
                let err2 = String::from_utf8_lossy(&out2.stderr).to_string();
                return Err(DomainError::RenderFailed(if err2.trim().is_empty() {
                    stderr
                } else {
                    err2
                }));
            }
            // Tools present but build failed — do not silently concat files (hides broken refs).
            return Err(DomainError::RenderFailed(stderr));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let fallback = fallback_overlay(&root)?;
            Ok(RenderResult {
                yaml: fallback,
                render_path: format!("kustomize-fallback:{overlay}"),
                files: Vec::new(),
            })
        }
        Err(e) => Err(DomainError::RenderFailed(e.to_string())),
    }
}

fn fallback_overlay(root: &PathBuf) -> Result<String, DomainError> {
    let all = root.join("all.yaml");
    if all.exists() {
        return std::fs::read_to_string(all).map_err(|e| DomainError::RenderFailed(e.to_string()));
    }
    // Concatenate *.yaml in overlay directory (excluding kustomization.yaml)
    let mut files: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| DomainError::RenderFailed(e.to_string()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            (name.ends_with(".yaml") || name.ends_with(".yml"))
                && !name.starts_with("kustomization")
        })
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(DomainError::RenderFailed(
            "kustomize tools missing and no fallback manifests found".into(),
        ));
    }
    let mut docs = Vec::new();
    for f in files {
        docs.push(
            std::fs::read_to_string(f).map_err(|e| DomainError::RenderFailed(e.to_string()))?,
        );
    }
    Ok(docs.join("\n---\n"))
}
