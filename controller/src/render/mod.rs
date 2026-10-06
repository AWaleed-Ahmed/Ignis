pub mod common;
pub mod helm;
pub mod kustomize;
pub mod yaml;

use crate::domain::errors::DomainError;
use crate::domain::models::{ManifestSpec, RenderedFile};

pub struct RenderResult {
    pub yaml: String,
    pub render_path: String,
    pub files: Vec<RenderedFile>,
}

pub fn render(workspace: &str, manifests: &ManifestSpec) -> Result<RenderResult, DomainError> {
    render_with_deadline(
        workspace,
        manifests,
        crate::deadline::Deadline::new(std::time::Duration::from_secs(120)),
    )
}

pub fn render_with_deadline(
    workspace: &str,
    manifests: &ManifestSpec,
    deadline: crate::deadline::Deadline,
) -> Result<RenderResult, DomainError> {
    deadline.remaining()?;
    match manifests.manifest_type.as_str() {
        "yaml" => yaml::render_yaml(workspace, manifests),
        "helm" => helm::render_helm_with_deadline(workspace, manifests, deadline),
        "kustomize" => kustomize::render_kustomize_with_deadline(workspace, manifests, deadline),
        other => Err(DomainError::InvalidRequest(format!(
            "unsupported manifests.type: {other}"
        ))),
    }
}
