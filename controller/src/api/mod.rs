mod handlers;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tower_http::trace::TraceLayer;

use crate::domain::service::SandboxService;

pub fn router(service: Arc<SandboxService>) -> Router {
    Router::new()
        .route("/health", get(handlers::health))
        .route("/v1/sandboxes", post(handlers::create_sandbox))
        .route(
            "/v1/sandboxes/{sandbox_id}/deploy",
            post(handlers::deploy_revision),
        )
        .route(
            "/v1/sandboxes/{sandbox_id}/observe",
            post(handlers::observe_failure),
        )
        .route(
            "/v1/sandboxes/{sandbox_id}/validate",
            post(handlers::run_validation),
        )
        .route(
            "/v1/sandboxes/{sandbox_id}/finalize",
            post(handlers::finalize_result),
        )
        .route(
            "/v1/sandboxes/{sandbox_id}/result",
            get(handlers::get_result),
        )
        .route(
            "/v1/sandboxes/{sandbox_id}/destroy",
            post(handlers::destroy_sandbox),
        )
        .route("/v1/admin/force-cleanup", post(handlers::force_cleanup))
        .layer(TraceLayer::new_for_http())
        .with_state(service)
}
