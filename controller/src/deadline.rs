//! A monotonic budget shared by sequential deployment stages.
use crate::domain::errors::DomainError;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub struct Deadline(Instant);

impl Deadline {
    pub fn new(budget: Duration) -> Self {
        Self(Instant::now() + budget)
    }

    pub fn remaining(self) -> Result<Duration, DomainError> {
        let remaining = self.0.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            Err(DomainError::Timeout("execution deadline exhausted".into()))
        } else {
            Ok(remaining)
        }
    }

    pub fn cap(self, maximum: Duration) -> Result<Duration, DomainError> {
        Ok(self.remaining()?.min(maximum))
    }
}
