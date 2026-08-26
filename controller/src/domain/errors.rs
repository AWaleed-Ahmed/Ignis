use thiserror::Error;

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("sandbox not found: {0}")]
    NotFound(String),

    #[error("sandbox conflict: {0}")]
    Conflict(String),

    #[error("policy blocked: {0}")]
    PolicyBlocked(String),

    #[error("render failed: {0}")]
    RenderFailed(String),

    #[error("deploy failed: {0}")]
    DeployFailed(String),

    #[error("observation failed: {0}")]
    ObservationFailed(String),

    #[error("validation unavailable: {0}")]
    ValidationUnavailable(String),

    #[error("validation failed: {0}")]
    ValidationFailed(String),

    #[error("cluster unavailable: {0}")]
    ClusterUnavailable(String),

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl DomainError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::PolicyBlocked(_) => "policy_blocked",
            Self::RenderFailed(_) => "render_failed",
            Self::DeployFailed(_) => "deploy_failed",
            Self::ObservationFailed(_) => "observation_failed",
            Self::ValidationUnavailable(_) => "validation_unavailable",
            Self::ValidationFailed(_) => "validation_failed",
            Self::ClusterUnavailable(_) => "cluster_unavailable",
            Self::Timeout(_) => "timeout",
            Self::Internal(_) => "internal",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::ClusterUnavailable(_) | Self::Timeout(_) | Self::Internal(_)
        )
    }
}
