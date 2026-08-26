pub mod signatures;

#[cfg(test)]
#[path = "signatures_tests.rs"]
mod signatures_tests;

use crate::domain::models::FailureSignature;
use crate::k8s::WorkloadObservation;

pub fn observe(observation: &WorkloadObservation, rendered_yaml: Option<&str>) -> FailureSignature {
    signatures::analyze_observation(observation, rendered_yaml)
}

pub use signatures::analyze_rendered_yaml;
