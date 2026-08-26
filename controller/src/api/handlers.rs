use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::domain::errors::DomainError;
use crate::domain::models::*;
use crate::domain::service::SandboxService;

pub async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "raphael-sandbox-controller"
    }))
}

pub async fn create_sandbox(
    State(service): State<Arc<SandboxService>>,
    Json(req): Json<CreateSandboxRequest>,
) -> Result<Json<CreateSandboxResponse>, ApiError> {
    Ok(Json(service.create_sandbox(req).await?))
}

pub async fn deploy_revision(
    State(service): State<Arc<SandboxService>>,
    Path(sandbox_id): Path<String>,
    Json(req): Json<DeployRevisionRequest>,
) -> Result<Json<DeployRevisionResponse>, ApiError> {
    Ok(Json(service.deploy_revision(&sandbox_id, req).await?))
}

pub async fn observe_failure(
    State(service): State<Arc<SandboxService>>,
    Path(sandbox_id): Path<String>,
    Json(req): Json<ObserveFailureRequest>,
) -> Result<Json<ObserveFailureResponse>, ApiError> {
    Ok(Json(service.observe_failure(&sandbox_id, req).await?))
}

pub async fn run_validation(
    State(service): State<Arc<SandboxService>>,
    Path(sandbox_id): Path<String>,
    Json(req): Json<RunValidationRequest>,
) -> Result<Json<ValidationResults>, ApiError> {
    Ok(Json(service.run_validation(&sandbox_id, req).await?))
}

pub async fn finalize_result(
    State(service): State<Arc<SandboxService>>,
    Path(sandbox_id): Path<String>,
    Json(req): Json<FinalizeResultRequest>,
) -> Result<Json<FinalizeResultResponse>, ApiError> {
    Ok(Json(service.finalize_result(&sandbox_id, req).await?))
}

pub async fn get_result(
    State(service): State<Arc<SandboxService>>,
    Path(sandbox_id): Path<String>,
) -> Result<Json<ValidatedFixRecord>, ApiError> {
    Ok(Json(service.get_result(&sandbox_id)?))
}

pub async fn destroy_sandbox(
    State(service): State<Arc<SandboxService>>,
    Path(sandbox_id): Path<String>,
    Json(req): Json<DestroySandboxRequest>,
) -> Result<Json<DestroySandboxResponse>, ApiError> {
    Ok(Json(service.destroy_sandbox(&sandbox_id, req).await?))
}

pub async fn force_cleanup(
    State(service): State<Arc<SandboxService>>,
    Json(req): Json<ForceCleanupRequest>,
) -> Result<Json<ForceCleanupResponse>, ApiError> {
    Ok(Json(service.force_cleanup(req).await?))
}

pub struct ApiError(DomainError);

impl From<DomainError> for ApiError {
    fn from(value: DomainError) -> Self {
        Self(value)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            DomainError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            DomainError::NotFound(_) => StatusCode::NOT_FOUND,
            DomainError::Conflict(_) => StatusCode::CONFLICT,
            DomainError::PolicyBlocked(_) => StatusCode::UNPROCESSABLE_ENTITY,
            DomainError::ValidationFailed(_) => StatusCode::UNPROCESSABLE_ENTITY,
            DomainError::ValidationUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            DomainError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            DomainError::ClusterUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: self.0.code().to_string(),
                message: self.0.to_string(),
                retryable: self.0.retryable(),
                details: None,
                sandbox_id: None,
                run_id: None,
            },
        };
        (status, Json(body)).into_response()
    }
}
