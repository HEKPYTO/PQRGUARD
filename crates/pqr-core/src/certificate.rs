use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::math::{beta_for, misses_allowed, MathError};

pub const SCHEMA: &str = "pqr.guard/v0.1";

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
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
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Error, PartialEq)]
pub enum CertificateError {
    #[error("unexpected certificate schema")]
    Schema,
    #[error("certificate status is not certified")]
    Status,
    #[error("dimension must be at least 3")]
    Dimension,
    #[error("stages must be nonempty, strictly increasing, and within dim")]
    Stages,
    #[error(transparent)]
    Target(#[from] TargetError),
    #[error("non-finite claim value")]
    NonFinite,
    #[error("claim kind mismatch")]
    ClaimKind,
    #[error("claim theorem mismatch")]
    ClaimTheorem,
    #[error("claim beta mismatch")]
    ClaimBeta,
    #[error("claim m_allowed mismatch")]
    ClaimMAllowed,
    #[error("claim probability space mismatch")]
    ClaimProbabilitySpace,
    #[error("claim expected recall mismatch")]
    ClaimExpectedRecall,
    #[error("assumption set mismatch")]
    Assumptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

    pub fn verify(&self) -> Result<(), CertificateError> {
        if self.schema != SCHEMA {
            return Err(CertificateError::Schema);
        }
        if self.status != Status::Certified {
            return Err(CertificateError::Status);
        }
        if self.dim < 3 {
            return Err(CertificateError::Dimension);
        }
        if self.stages.is_empty()
            || self
                .stages
                .iter()
                .any(|s| s.m_bits == 0 || s.m_bits > self.dim)
            || self.stages.windows(2).any(|w| w[0].m_bits >= w[1].m_bits)
        {
            return Err(CertificateError::Stages);
        }
        let target = Target::new(self.k, self.tau, self.delta)?;
        if !self.claim.beta.is_finite() || !self.claim.expected_recall.is_finite() {
            return Err(CertificateError::NonFinite);
        }
        if self.claim.kind != "per_query_recall" {
            return Err(CertificateError::ClaimKind);
        }
        let theorem = if target.tau >= 1.0 { "Thm2" } else { "Thm3" };
        if self.claim.theorem != theorem {
            return Err(CertificateError::ClaimTheorem);
        }
        if !claim_close(self.claim.beta, target.beta()) {
            return Err(CertificateError::ClaimBeta);
        }
        if self.claim.m_allowed != target.m_allowed() {
            return Err(CertificateError::ClaimMAllowed);
        }
        if self.claim.probability_space != "secret_random_rotation_P" {
            return Err(CertificateError::ClaimProbabilitySpace);
        }
        if !claim_close(self.claim.expected_recall, target.expected_recall()) {
            return Err(CertificateError::ClaimExpectedRecall);
        }
        let expected = [
            "queries_independent_of_P",
            "updates_independent_of_P",
            "seed_secret",
            "nonadaptive",
            "target_fixed_before_execution",
        ];
        let actual: BTreeSet<&str> = self.assumptions.iter().map(String::as_str).collect();
        if self.assumptions.len() != expected.len()
            || actual.len() != expected.len()
            || !expected.iter().all(|a| actual.contains(*a))
        {
            return Err(CertificateError::Assumptions);
        }
        Ok(())
    }
}

const CLAIM_TOLERANCE: f64 = 1e-12;

fn claim_close(actual: f64, expected: f64) -> bool {
    let scale = expected.abs().max(1.0);
    (actual - expected).abs() <= CLAIM_TOLERANCE * scale
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
