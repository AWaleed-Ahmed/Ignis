//! Conservative classification of current, container-scoped kubelet pull evidence.
use crate::k8s::{ObservationSource, WorkloadObservation};
use crate::observe::signatures::AnalyzedSignature;
use std::collections::BTreeSet;

fn cause(message: &str) -> &'static str {
    let text = message.to_lowercase();
    if text.contains("may require") || text.contains("does not exist or") {
        return "unknown";
    }
    let mut causes = BTreeSet::new();
    for (label, markers) in [
        (
            "auth",
            &[
                "unauthorized",
                "authentication required",
                "authorization failed",
                "access denied",
                "insufficient_scope",
                "forbidden",
            ][..],
        ),
        (
            "network",
            &[
                "no such host",
                "dial tcp",
                "connection refused",
                "connection reset",
                "network is unreachable",
                "x509:",
                "tls handshake",
                "i/o timeout",
            ][..],
        ),
        (
            "rate_limited",
            &["toomanyrequests", "too many requests", "rate limit", "429"][..],
        ),
        (
            "not_found",
            &[
                "manifest unknown",
                "manifest_unknown",
                "failed to resolve reference",
                "not found",
            ][..],
        ),
    ] {
        if markers.iter().any(|marker| text.contains(marker)) {
            // Resolution errors are not sufficient without an explicit manifest absence.
            if label != "not_found"
                || text.contains("manifest unknown")
                || text.contains("manifest_unknown")
                || (text.contains("failed to resolve reference")
                    && text.contains("not found")
                    && !text.contains("404"))
            {
                causes.insert(label);
            }
        }
    }
    if causes.len() == 1 {
        causes.into_iter().next().unwrap()
    } else {
        "unknown"
    }
}

pub fn analyze(obs: &WorkloadObservation) -> Option<AnalyzedSignature> {
    let mut failures = Vec::new();
    for pod in &obs.pods {
        for c in &pod.container_statuses {
            let Some(reason @ ("ImagePullBackOff" | "ErrImagePull")) = c.waiting_reason.as_deref()
            else {
                continue;
            };
            let mut messages = Vec::new();
            if c.message_complete {
                if let Some(message) = c.waiting_message.as_deref() {
                    messages.push(message);
                }
            }
            // Events only supplement a status when they identify this current
            // Pod UID and quote the original image. Namespace-wide guesses fail closed.
            if let (Some(uid), Some(created), Some(image)) = (&pod.uid, pod.created_at, &c.image) {
                let quoted_image = format!("\"{image}\"");
                for event in &obs.events {
                    if event.message_complete && event.involved_kind == "Pod" && event.involved_uid.as_ref() == Some(uid)
                        && event.involved_name == pod.name && event.observed_at.is_some_and(|time| time >= created && time >= chrono::Utc::now() - chrono::Duration::minutes(5))
                        && event.message.contains(&quoted_image)
                        // An image shared by two containers does not identify which failed.
                        && c.event_image_unique
                    {
                        messages.push(event.message.as_str());
                    }
                }
            }
            let cause = if !c.image_current || (c.waiting_message.is_some() && !c.message_complete)
            {
                "unknown"
            } else {
                cause(&messages.join("\n"))
            };
            let class = match cause {
                "not_found" => "bad_image_reference",
                "auth" => "auth_denied",
                "network" => "network_error",
                "rate_limited" => "dependency_timeout",
                _ => "unknown",
            };
            let image = c.image.as_deref().unwrap_or("");
            let (kind, name) = pod
                .deployment
                .as_deref()
                .map(|name| ("Deployment", name))
                .unwrap_or(("Pod", &pod.name));
            let container_type = if c.is_init { "init" } else { "regular" };
            let mut attributes = serde_json::Map::new();
            attributes.insert("image".into(), image.into());
            attributes.insert("image_pull_cause".into(), cause.into());
            attributes.insert(
                "evidence_source".into(),
                if obs.source == ObservationSource::Runtime {
                    "runtime"
                } else {
                    "mock_fixture"
                }
                .into(),
            );
            attributes.insert("owner_verified".into(), pod.deployment.is_some().into());
            attributes.insert("container_type".into(), container_type.into());
            let key =
                serde_json::to_string(&(kind, name, container_type, c.name.as_str(), image, cause))
                    .unwrap();
            failures.push(AnalyzedSignature {
                class: class.into(),
                key: format!("image_pull:{key}"),
                reason: reason.into(),
                message: Some(format!("image pull cause: {cause}")),
                resource_kind: kind.into(),
                resource_name: name.into(),
                container: Some(c.name.clone()),
                attributes,
                summary: Some(format!("image pull failure ({cause})")),
                confidence: if cause == "unknown" { 0.3 } else { 0.9 },
            });
        }
    }
    failures.sort_by(|a, b| a.key.cmp(&b.key));
    failures.into_iter().next()
}

/// Bound excerpts by characters and remove URLs/credentials before storage.
pub(crate) fn safe_excerpt(text: &str) -> String {
    use std::sync::OnceLock;
    static SENSITIVE: OnceLock<regex::Regex> = OnceLock::new();
    let bounded: String = text.chars().take(2048).collect();
    let pattern = SENSITIVE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(?:https?://\S+|(?:authorization|password|token|api[_-]?key)\s*[:=]\s*\S+)",
        )
        .expect("static sanitization pattern")
    });
    pattern.replace_all(&bounded, "[redacted]").into_owned()
}
