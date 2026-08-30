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
    match manifests.manifest_type.as_str() {
        "yaml" => yaml::render_yaml(workspace, manifests),
        "helm" => helm::render_helm(workspace, manifests),
        "kustomize" => kustomize::render_kustomize(workspace, manifests),
        other => Err(DomainError::InvalidRequest(format!(
            "unsupported manifests.type: {other}"
        ))),
    }
}
