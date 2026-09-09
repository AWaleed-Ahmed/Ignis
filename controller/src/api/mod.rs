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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    use crate::k8s::mock::MockCluster;
    use crate::state::recovery::RecoveryStore;
    use crate::state::registry::SandboxRegistry;

    #[tokio::test]
    async fn guarded_http_replay_executes_controller_side_effect_once() {
        let backend = Arc::new(MockCluster::new());
        let service = Arc::new(SandboxService::with_recovery(
            backend.clone(),
            Arc::new(SandboxRegistry::new()),
            Arc::new(RecoveryStore::in_memory()),
        ));
        let body = json!({"run_id":"guard-run","tenant_id":"tenant","repository":{"owner":"o","name":"n","clone_url":"https://example.com/n.git"},"commit_sha":"0123456789abcdef"}).to_string();
        let request = || {
            Request::builder()
                .method("POST")
                .uri("/v1/sandboxes")
                .header("content-type", "application/json")
                .header("x-raphael-connector-action-id", "action-guard-1")
                .body(Body::from(body.clone()))
                .unwrap()
        };
        let first = router(service.clone()).oneshot(request()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = first.into_body().collect().await.unwrap().to_bytes();
        let second = router(service).oneshot(request()).await.unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(
            first_body,
            second.into_body().collect().await.unwrap().to_bytes()
        );
        assert_eq!(
            backend.create_call_count(),
            1,
            "controller guard must replay without a second namespace create"
        );
    }
}
