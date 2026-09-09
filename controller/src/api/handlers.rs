use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;
use std::future::Future;

use crate::domain::errors::DomainError;
use crate::domain::ids::sandbox_id_from_run;
use crate::domain::models::*;
use crate::domain::service::SandboxService;
use crate::state::recovery::value_hash;

async fn guarded<T, F>(
    service: &SandboxService,
    headers: &HeaderMap,
    scope: String,
    request: serde_json::Value,
    execute: F,
) -> Result<T, ApiError>
where
    T: Serialize + DeserializeOwned,
    F: Future<Output = Result<T, DomainError>>,
{
    let Some(action_id) = headers
        .get("x-raphael-connector-action-id")
        .and_then(|v| v.to_str().ok())
    else {
        return Ok(execute.await?);
    };
    let hash = value_hash(&request);
    if let Some(response) = service
        .recovery()
        .controller_receipt(&scope, action_id, &hash)
        .map_err(DomainError::InvalidRequest)?
    {
        return serde_json::from_value(response)
            .map_err(|e| ApiError(DomainError::Internal(e.to_string())));
    }
    let response = execute.await?;
    service
        .recovery()
        .save_controller_receipt(
            &scope,
            action_id,
            hash,
            serde_json::to_value(&response).map_err(|e| DomainError::Internal(e.to_string()))?,
        )
        .map_err(DomainError::Internal)?;
    Ok(response)
}

pub async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "raphael-sandbox-controller"
    }))
}

pub async fn create_sandbox(
    State(service): State<Arc<SandboxService>>,
    headers: HeaderMap,
    Json(req): Json<CreateSandboxRequest>,
) -> Result<Json<CreateSandboxResponse>, ApiError> {
    let scope = sandbox_id_from_run(&req.run_id);
    let body =
        serde_json::to_value(&req).map_err(|e| ApiError(DomainError::Internal(e.to_string())))?;
    Ok(Json(
        guarded(&service, &headers, scope, body, service.create_sandbox(req)).await?,
    ))
}

pub async fn deploy_revision(
    State(service): State<Arc<SandboxService>>,
    Path(sandbox_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<DeployRevisionRequest>,
) -> Result<Json<DeployRevisionResponse>, ApiError> {
    let body =
        serde_json::to_value(&req).map_err(|e| ApiError(DomainError::Internal(e.to_string())))?;
    Ok(Json(
        guarded(
            &service,
            &headers,
            sandbox_id.clone(),
            body,
            service.deploy_revision(&sandbox_id, req),
        )
        .await?,
    ))
}

pub async fn observe_failure(
    State(service): State<Arc<SandboxService>>,
    Path(sandbox_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<ObserveFailureRequest>,
) -> Result<Json<ObserveFailureResponse>, ApiError> {
    let body =
        serde_json::to_value(&req).map_err(|e| ApiError(DomainError::Internal(e.to_string())))?;
    Ok(Json(
        guarded(
            &service,
            &headers,
            sandbox_id.clone(),
            body,
            service.observe_failure(&sandbox_id, req),
        )
        .await?,
    ))
}

pub async fn run_validation(
    State(service): State<Arc<SandboxService>>,
    Path(sandbox_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<RunValidationRequest>,
) -> Result<Json<ValidationResults>, ApiError> {
    let body =
        serde_json::to_value(&req).map_err(|e| ApiError(DomainError::Internal(e.to_string())))?;
    Ok(Json(
        guarded(
            &service,
            &headers,
            sandbox_id.clone(),
            body,
            service.run_validation(&sandbox_id, req),
        )
        .await?,
    ))
}

pub async fn finalize_result(
    State(service): State<Arc<SandboxService>>,
    Path(sandbox_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<FinalizeResultRequest>,
) -> Result<Json<FinalizeResultResponse>, ApiError> {
    let body =
        serde_json::to_value(&req).map_err(|e| ApiError(DomainError::Internal(e.to_string())))?;
    Ok(Json(
        guarded(
            &service,
            &headers,
            sandbox_id.clone(),
            body,
            service.finalize_result(&sandbox_id, req),
        )
        .await?,
    ))
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
