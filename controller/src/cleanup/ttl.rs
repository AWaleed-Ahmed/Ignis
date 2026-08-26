use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::time::sleep;

use crate::domain::service::SandboxService;

pub struct TtlReaper {
    service: Arc<SandboxService>,
    interval: Duration,
}

impl TtlReaper {
    pub fn new(service: Arc<SandboxService>, interval: Duration) -> Self {
        Self { service, interval }
    }

    pub async fn run(&self) {
        loop {
            let now = Utc::now();
            if let Err(e) = self.service.reap_expired(now).await {
                tracing::warn!(error = %e, "ttl reaper iteration failed");
            }
            sleep(self.interval).await;
        }
    }
}
