use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::certificate::Target;
use crate::index::{IndexError, PqrIndex, QueryOptions};

#[derive(Debug, Clone)]
pub struct AuditConfig {
    pub rotations: u64,
    pub nprobe: usize,
    pub master_seed: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryAudit {
    pub query: usize,
    pub failures: u64,
    pub trials: u64,
    pub cp_upper: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
    pub schema: String,
    pub index_id: String,
    pub rotations: u64,
    pub queries: usize,
    pub k: usize,
    pub tau: f64,
    pub delta: f64,
    pub trials: u64,
    pub failures: u64,
    pub failure_rate: f64,
    pub max_cp_upper: f64,
    pub queries_over_delta: usize,
    pub gate: String,
    pub verdict: String,
    pub inference: String,
    pub per_query: Vec<QueryAudit>,
}

#[derive(Debug, Error)]
pub enum AuditError {
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error("queries are not a multiple of dim")]
    Queries,
    #[error("ground truth shape does not match queries and k")]
    GroundTruth,
}

pub fn clopper_pearson_upper(failures: u64, trials: u64) -> f64 {
    if trials == 0 {
        return 1.0;
    }
    if failures == 0 {
        return 1.0 - 0.05f64.powf(1.0 / trials as f64);
    }
    crate::math::beta_quantile_general((failures + 1) as f64, (trials - failures) as f64, 0.95)
        .unwrap_or(1.0)
}

fn rotation_seed(master: &[u8; 32], rotation: u64) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"pqr.guard.audit.rotation");
    h.update(master);
    h.update(&rotation.to_le_bytes());
    *h.finalize().as_bytes()
}

pub fn audit(
    index: &mut PqrIndex,
    queries: &[f32],
    target: Target,
    cfg: &AuditConfig,
    ground_truth: Option<&[u64]>,
) -> Result<AuditReport, AuditError> {
    let dim = index.dim();
    if queries.is_empty() || queries.len() % dim != 0 {
        return Err(AuditError::Queries);
    }
    let nq = queries.len() / dim;
    let k = target.k;
    let owned_truth;
    let truth = match ground_truth {
        Some(gt) => {
            if gt.len() != nq * k {
                return Err(AuditError::GroundTruth);
            }
            gt
        }
        None => {
            owned_truth = brute_truth(index.vectors(), dim, queries, nq, k);
            &owned_truth
        }
    };
    if cfg.rotations == 0 {
        return Err(AuditError::GroundTruth);
    }
    let original_id = index.index_id().to_string();
    let reasons = Vec::new();
    let stage_eps = index.stage_eps(&target)?;
    let mut failures = vec![0u64; nq];
    for rotation in 0..cfg.rotations {
        let seed = rotation_seed(&cfg.master_seed, rotation);
        index.reencode(&seed);
        for qi in 0..nq {
            let q = &queries[qi * dim..(qi + 1) * dim];
            let outcome = index.query(
                q,
                &QueryOptions {
                    target,
                    nprobe: cfg.nprobe,
                    reasons: reasons.clone(),
                    stage_eps: Some(stage_eps.clone()),
                },
            )?;
            let hits = match outcome {
                crate::index::QueryOutcome::Certified { hits, .. } => hits,
                crate::index::QueryOutcome::Degraded { hits, .. } => hits,
            };
            let wanted = &truth[qi * k..(qi + 1) * k];
            let found = hits.iter().filter(|h| wanted.contains(&h.id)).count();
            if (found as f64) / (k as f64) < target.tau - 1e-12 {
                failures[qi] += 1;
            }
        }
    }
    let mut per_query = Vec::with_capacity(nq);
    let mut total_failures = 0u64;
    let mut max_cp: f64 = 0.0;
    let mut over = 0usize;
    for (qi, f) in failures.iter().enumerate() {
        let cp = clopper_pearson_upper(*f, cfg.rotations);
        total_failures += *f;
        max_cp = max_cp.max(cp);
        if cp > target.delta {
            over += 1;
        }
        per_query.push(QueryAudit {
            query: qi,
            failures: *f,
            trials: cfg.rotations,
            cp_upper: cp,
        });
    }
    let trials = nq as u64 * cfg.rotations;
    let rate = total_failures as f64 / trials as f64;
    let allowed_over = (nq as f64 * 0.005).floor() as usize;
    let per_query_gate = cfg.rotations >= 1000;
    let pass = if per_query_gate {
        rate <= target.delta && max_cp <= 1.5 * target.delta && over <= allowed_over
    } else {
        rate <= target.delta
    };
    Ok(AuditReport {
        schema: crate::certificate::SCHEMA.to_string(),
        index_id: original_id,
        rotations: cfg.rotations,
        queries: nq,
        k,
        tau: target.tau,
        delta: target.delta,
        trials,
        failures: total_failures,
        failure_rate: rate,
        max_cp_upper: max_cp,
        queries_over_delta: over,
        gate: if per_query_gate {
            "per_query_cp"
        } else {
            "pooled"
        }
        .to_string(),
        verdict: if pass { "pass" } else { "fail" }.to_string(),
        inference: "falsification-only".to_string(),
        per_query,
    })
}

fn brute_truth(vectors: &[f32], dim: usize, queries: &[f32], nq: usize, k: usize) -> Vec<u64> {
    let mut out = Vec::with_capacity(nq * k);
    for qi in 0..nq {
        let q = &queries[qi * dim..(qi + 1) * dim];
        let mut all: Vec<(f64, u64)> = (0..vectors.len() / dim)
            .map(|id| {
                let mut s = 0.0;
                for j in 0..dim {
                    let d = vectors[id * dim + j] as f64 - q[j] as f64;
                    s += d * d;
                }
                (s, id as u64)
            })
            .collect();
        all.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        out.extend(all.into_iter().take(k).map(|(_, id)| id));
    }
    out
}
