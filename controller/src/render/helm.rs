use crate::deadline::Deadline;
use std::path::PathBuf;
use std::time::Duration;

use tokio::runtime::Handle;

use crate::domain::errors::DomainError;
use crate::domain::models::ManifestSpec;
use crate::render::RenderResult;

pub fn render_helm(workspace: &str, manifests: &ManifestSpec) -> Result<RenderResult, DomainError> {
    render_helm_with_deadline(
        workspace,
        manifests,
        Deadline::new(Duration::from_secs(120)),
    )
}

pub fn render_helm_with_deadline(
    workspace: &str,
    manifests: &ManifestSpec,
    deadline: Deadline,
) -> Result<RenderResult, DomainError> {
    // Prefer async runtime if present; helm template is sync CLI.
    if Handle::try_current().is_ok() {
        // We're on a runtime; use block_in_place for subprocess.
        return tokio::task::block_in_place(|| render_helm_sync(workspace, manifests, deadline));
    }
    render_helm_sync(workspace, manifests, deadline)
}

fn render_helm_sync(
    workspace: &str,
    manifests: &ManifestSpec,
    deadline: Deadline,
) -> Result<RenderResult, DomainError> {
    let chart = manifests
        .chart
        .as_deref()
        .ok_or_else(|| DomainError::InvalidRequest("manifests.chart required for helm".into()))?;
    let chart_path = PathBuf::from(workspace).join(chart);
    if !chart_path.exists() {
        return Err(DomainError::RenderFailed(format!(
            "helm chart not found: {}",
            chart_path.display()
        )));
    }

    let release = manifests
        .release_name
        .clone()
        .unwrap_or_else(|| "raphael".into());

    let mut args = vec![
        "template".to_string(),
        release.clone(),
        chart_path.to_string_lossy().to_string(),
    ];
    if let Some(values) = &manifests.values {
        for v in values {
            let vp = PathBuf::from(workspace).join(v);
            args.push("-f".into());
            args.push(vp.to_string_lossy().to_string());
        }
    }

    let mut lint_cmd = std::process::Command::new("helm");
    lint_cmd.arg("lint").arg(&chart_path);
    if let Some(values) = &manifests.values {
        for v in values {
            let vp = PathBuf::from(workspace).join(v);
            lint_cmd.arg("-f").arg(&vp);
        }
    }
    let lint = crate::process::output(&mut lint_cmd, deadline.remaining()?);

    match lint {
        Ok(out) if !out.status.success() => {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let combined = format!("{stdout}\n{stderr}");
            // Schema / type errors and hard lint failures
            if combined.to_lowercase().contains("error")
                || combined.to_lowercase().contains("invalid type")
                || combined.to_lowercase().contains("values don't meet")
            {
                return Err(DomainError::RenderFailed(format!(
                    "helm lint/schema failed: {combined}"
                )));
            }
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::TimedOut {
                return Err(DomainError::Timeout("helm lint timed out".into()));
            }
            // Fallback: if helm is not installed, try to render a simple charts/templates concat for demos
            if e.kind() == std::io::ErrorKind::NotFound {
                if chart_path.join("values.schema.json").exists() {
                    return Err(DomainError::RenderFailed(
                        "helm binary required to validate values.schema.json".into(),
                    ));
                }
                return fallback_chart_concat(workspace, chart, &release);
            }
            return Err(DomainError::RenderFailed(e.to_string()));
        }
        _ => {}
    }

    let output = crate::process::output(
        std::process::Command::new("helm").args(&args),
        deadline.remaining()?,
    )
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::TimedOut {
            DomainError::Timeout("helm template timed out".into())
        } else {
            DomainError::RenderFailed(e.to_string())
        }
    })?;

    if !output.status.success() {
        return Err(DomainError::RenderFailed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let yaml = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(RenderResult {
        yaml,
        render_path: format!("helm:{}:{}", chart, release),
        files: Vec::new(),
    })
}

fn fallback_chart_concat(
    workspace: &str,
    chart: &str,
    release: &str,
) -> Result<RenderResult, DomainError> {
    let templates = PathBuf::from(workspace).join(chart).join("templates");
    if !templates.exists() {
        return Err(DomainError::RenderFailed(
            "helm not installed and no templates/ fallback found".into(),
        ));
    }
    // Extremely small demo fallback: concatenate templates (no Go templating).
    // Suitable only for scenarios that ship already-expanded YAML under templates/.
    let mut files: Vec<_> = walkdir::WalkDir::new(&templates)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.into_path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    let mut docs = Vec::new();
    for f in files {
        docs.push(
            std::fs::read_to_string(&f).map_err(|e| DomainError::RenderFailed(e.to_string()))?,
        );
    }
    Ok(RenderResult {
        yaml: docs.join("\n---\n"),
        render_path: format!("helm-fallback:{}:{}", chart, release),
        files: Vec::new(),
    })
}
