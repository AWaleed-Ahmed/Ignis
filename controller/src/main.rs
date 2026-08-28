use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use raphael_sandbox_controller::api::router;
use raphael_sandbox_controller::cleanup::ttl::TtlReaper;
use raphael_sandbox_controller::connector::{Connector, ConnectorConfig};
use raphael_sandbox_controller::domain::service::SandboxService;
use raphael_sandbox_controller::k8s::{create_backend, ClusterBackend};
use raphael_sandbox_controller::state::registry::SandboxRegistry;
use raphael_sandbox_controller::state::sqlite::SqliteStore;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .json()
        .init();

    let backend_name = env::var("RAPHAEL_CLUSTER_BACKEND").unwrap_or_else(|_| "mock".into());
    // Avoid colliding with kubectl's insecure default (http://127.0.0.1:8080) when kubeconfig is missing.
    let addr: SocketAddr = env::var("RAPHAEL_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:8090".into())
        .parse()?;

    if backend_name == "kind" || backend_name == "kubectl" || backend_name == "kubeconfig" {
        if env::var_os("KUBECONFIG").is_none() {
            let default_kube = dirs_kubeconfig();
            if default_kube.exists() {
                // leave default discovery to kubectl
            } else {
                tracing::warn!(
                    "no kubeconfig found at {}; kind bootstrap may not have run",
                    default_kube.display()
                );
            }
        }
        if env::var_os("RAPHAEL_KUBE_CONTEXT").is_none() && backend_name == "kind" {
            // Safe default for our bootstrap script cluster name.
            env::set_var("RAPHAEL_KUBE_CONTEXT", "kind-raphael-sandbox");
        }
    }

    let backend: Arc<dyn ClusterBackend> = create_backend(&backend_name)?;

    let sqlite_path = env::var("RAPHAEL_SQLITE_PATH").unwrap_or_else(|_| {
        let dir = env::var("RAPHAEL_DATA_DIR").unwrap_or_else(|_| ".raphael-data".into());
        format!("{dir}/sandboxes.db")
    });
    let registry = match SqliteStore::open(&sqlite_path) {
        Ok(store) => {
            tracing::info!(path = %sqlite_path, "sqlite persistence enabled");
            Arc::new(SandboxRegistry::with_store(Arc::new(store)).map_err(|e| anyhow::anyhow!(e))?)
        }
        Err(e) => {
            tracing::warn!(error = %e, "sqlite unavailable; continuing in-memory only");
            Arc::new(SandboxRegistry::new())
        }
    };

    let _ = raphael_sandbox_controller::artifacts::ensure_root();

    let service = Arc::new(SandboxService::new(backend.clone(), registry.clone()));

    if let Some(config) = ConnectorConfig::from_env()? {
        tracing::info!(dispatch_url = %config.dispatch_url, controller_url = %config.controller_url, "starting outbound connector");
        let connector = Arc::new(Connector::from_config(config));
        tokio::spawn(connector.run_forever());
    }

    let reaper = TtlReaper::new(service.clone(), Duration::from_secs(30));
    tokio::spawn(async move {
        reaper.run().await;
    });

    let app = router(service);

    tracing::info!(%addr, backend = %backend_name, "sandbox controller listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn dirs_kubeconfig() -> std::path::PathBuf {
    if let Some(home) = env::var_os("HOME") {
        return std::path::PathBuf::from(home).join(".kube/config");
    }
    std::path::PathBuf::from("/root/.kube/config")
}
