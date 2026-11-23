use faer::Mat;
use thiserror::Error;

use crate::code::{code_from, prefix_stats, prune};
use crate::math::{beta_for, beta_quantile, misses_allowed, MathError};

#[derive(Debug, Clone)]
pub struct ControlConfig {
    pub dim: usize,
    pub k: usize,
    pub tau: f64,
    pub delta: f64,
    pub rotations: u64,
    pub seed: u64,
    pub rho: f64,
    pub eta: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlReport {
    pub trials: u64,
    pub failures: u64,
    pub rate: f64,
}

#[derive(Debug, Error, PartialEq)]
pub enum ControlError {
    #[error(transparent)]
    Math(#[from] MathError),
    #[error("k must be a multiple of m+1")]
    Groups,
    #[error("2G must fit in the dimension")]
    Dimension,
    #[error("rho must be in (0, 1)")]
    Rho,
    #[error("eta must be positive")]
    Eta,
}

pub fn proposition_one_failures(
    cfg: &ControlConfig,
    eps_override: Option<f64>,
) -> Result<ControlReport, ControlError> {
    let beta = beta_for(cfg.tau, cfg.k, cfg.delta)?;
    let m = misses_allowed(cfg.tau, cfg.k);
    let per_group = m + 1;
    if cfg.k % per_group != 0 {
        return Err(ControlError::Groups);
    }
    let groups = cfg.k / per_group;
    if 2 * groups > cfg.dim {
        return Err(ControlError::Dimension);
    }
    if cfg.rho <= 0.0 || cfg.rho >= 1.0 {
        return Err(ControlError::Rho);
    }
    if cfg.eta <= 0.0 {
        return Err(ControlError::Eta);
    }
    let eps0 = match eps_override {
        Some(e) => e,
        None => beta_quantile(cfg.dim, beta)?,
    };

    let r = 1.0;
    let d = r - cfg.eta;
    let rho = cfg.rho;
    let phi = d / (2.0 * (1.0 - rho)).sqrt();
    let theta = 1.0 - r * r / (2.0 * phi * phi);
    let slack = (1.0 - theta.max(0.0) * theta.max(0.0)).sqrt();

    let mut o = Mat::<f64>::zeros(groups, cfg.dim);
    let mut q = Mat::<f64>::zeros(groups, cfg.dim);
    let perp = (1.0 - rho * rho).sqrt();
    for g in 0..groups {
        o[(g, 2 * g)] = 1.0;
        q[(g, 2 * g)] = rho;
        q[(g, 2 * g + 1)] = perp;
    }

    let mut failures = 0u64;
    for t in 0..cfg.rotations {
        let rotation = crate::rotation::Rotation::new(cfg.dim, cfg.seed.wrapping_add(t));
        let ro = rotation.apply_transpose_rows(&o);
        let rq = rotation.apply_transpose_rows(&q);
        let mut failed = false;
        for g in 0..groups {
            let o_row: Vec<f64> = (0..cfg.dim).map(|i| ro[(g, i)]).collect();
            let q_row: Vec<f64> = (0..cfg.dim).map(|i| rq[(g, i)]).collect();
            let code = code_from(&o_row);
            let Some(stats) = prefix_stats(&code, &o_row, &q_row, cfg.dim) else {
                continue;
            };
            if prune(&stats, theta, slack, eps0) {
                failed = true;
                break;
            }
        }
        if failed {
            failures += 1;
        }
    }
    Ok(ControlReport {
        trials: cfg.rotations,
        failures,
        rate: failures as f64 / cfg.rotations as f64,
    })
}
