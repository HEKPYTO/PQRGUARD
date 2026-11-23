use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::math::{beta_for, misses_allowed, MathError};

pub const SCHEMA: &str = "pqr.guard/v0.1";

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub(crate) k: usize,
    pub(crate) tau: f64,
    pub(crate) delta: f64,
}

#[derive(Debug, Error, PartialEq)]
pub enum TargetError {
    #[error("k must be positive")]
    K,
    #[error("tau must be in (0, 1]")]
    Tau,
    #[error("delta must be in (0, 1)")]
    Delta,
    #[error(transparent)]
    Math(#[from] MathError),
}

impl Target {
    pub fn new(k: usize, tau: f64, delta: f64) -> Result<Self, TargetError> {
        if k == 0 {
            return Err(TargetError::K);
        }
        if !(tau > 0.0 && tau <= 1.0) {
            return Err(TargetError::Tau);
        }
        if !(delta > 0.0 && delta < 1.0) {
            return Err(TargetError::Delta);
        }
        beta_for(tau, k, delta)?;
        Ok(Self { k, tau, delta })
    }

    pub fn k(&self) -> usize {
        self.k
    }

    pub fn tau(&self) -> f64 {
        self.tau
    }

    pub fn delta(&self) -> f64 {
        self.delta
    }

    pub fn beta(&self) -> f64 {
        beta_for(self.tau, self.k, self.delta).expect("validated target")
    }

    pub fn m_allowed(&self) -> usize {
        misses_allowed(self.tau, self.k)
    }

    pub fn expected_recall(&self) -> f64 {
        (1.0 - self.beta()).clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    #[serde(rename = "type")]
    pub kind: String,
    pub theorem: String,
    pub beta: f64,
    pub m_allowed: usize,
    pub probability_space: String,
    pub expected_recall: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage {
    pub m_bits: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Certified,
    Degraded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradedReason {
    PublicSeed,
    AdaptiveSequence,
    RotationDependentUpdate,
    ApproximateRerank,
    FilteredSearch,
    UnsupportedBackend,
    SeedMismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Certificate {
    pub schema: String,
    pub index_id: String,
    pub dim: usize,
    pub k: usize,
    pub tau: f64,
    pub delta: f64,
    pub claim: Claim,
    pub stages: Vec<Stage>,
    pub assumptions: Vec<String>,
    pub status: Status,
}

impl Certificate {
    pub fn certified(index_id: &str, dim: usize, target: &Target, stages: &[usize]) -> Self {
        Self {
            schema: SCHEMA.to_string(),
            index_id: index_id.to_string(),
            dim,
            k: target.k,
            tau: target.tau,
            delta: target.delta,
            claim: Claim {
                kind: "per_query_recall".to_string(),
                theorem: if target.tau >= 1.0 {
                    "Thm2".to_string()
                } else {
                    "Thm3".to_string()
                },
                beta: target.beta(),
                m_allowed: target.m_allowed(),
                probability_space: "secret_random_rotation_P".to_string(),
                expected_recall: target.expected_recall(),
            },
            stages: stages.iter().map(|m| Stage { m_bits: *m }).collect(),
            assumptions: vec![
                "queries_independent_of_P".to_string(),
                "updates_independent_of_P".to_string(),
                "seed_secret".to_string(),
                "nonadaptive".to_string(),
                "target_fixed_before_execution".to_string(),
            ],
            status: Status::Certified,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Degraded {
    pub index_id: String,
    pub dim: usize,
    pub k: usize,
    pub tau: f64,
    pub delta: f64,
    pub reasons: Vec<DegradedReason>,
    pub status: Status,
}

impl Degraded {
    pub fn new(index_id: &str, dim: usize, target: &Target, reasons: Vec<DegradedReason>) -> Self {
        Self {
            index_id: index_id.to_string(),
            dim,
            k: target.k,
            tau: target.tau,
            delta: target.delta,
            reasons,
            status: Status::Degraded,
        }
    }
}
