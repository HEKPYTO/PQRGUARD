use pqr_core::certificate::{DegradedReason, Target};
use pqr_core::index::{BuildConfig, IndexError, PqrIndex, QueryOptions, QueryOutcome};

fn data(n: usize, dim: usize) -> Vec<f32> {
    let mut state = 0x9e3779b97f4a7c15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };
    let centers = [[1.0f64, 0.0], [-1.0, 0.5], [0.0, -1.0]];
    let mut out = Vec::with_capacity(n * dim);
    for id in 0..n {
        let c = centers[id % centers.len()];
        for j in 0..dim {
            let base = c[j % 2] * 2.0;
            out.push((base + (next() - 0.5) * 0.8) as f32);
        }
    }
    out
}

fn brute(vectors: &[f32], dim: usize, q: &[f32], k: usize) -> Vec<u64> {
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
    all.into_iter().take(k).map(|(_, id)| id).collect()
}

fn build(dim: usize) -> (PqrIndex, Vec<f32>) {
    let vectors = data(1500, dim);
    let cfg = BuildConfig {
        partitions: 30,
        stages: vec![dim / 2, dim],
        iterations: 8,
    };
    let (index, _) = PqrIndex::build(&vectors, dim, &cfg, &[7u8; 32]).unwrap();
    (index, vectors)
}

#[test]
fn certified_queries_match_brute_force_when_exact() {
    let dim = 16;
    let (index, vectors) = build(dim);
    let target = Target::new(10, 1.0, 0.01).unwrap();
    let mut exact = 0;
    let queries = 40;
    for qi in 0..queries {
        let q: Vec<f32> = vectors[qi * dim..(qi + 1) * dim].to_vec();
        let outcome = index
            .query(
                &q,
                &QueryOptions {
                    target,
                    nprobe: 3,
                    reasons: Vec::new(),
                    stage_eps: None,
                },
            )
            .unwrap();
        let QueryOutcome::Certified { hits, certificate } = outcome else {
            panic!("expected certified");
        };
        assert_eq!(certificate.status, pqr_core::certificate::Status::Certified);
        let truth = brute(&vectors, dim, &q, 10);
        let got: Vec<u64> = hits.iter().map(|h| h.id).collect();
        if got == truth {
            exact += 1;
        }
    }
    assert!(exact >= queries * 9 / 10, "only {exact}/{queries} exact");
}

#[test]
fn save_load_roundtrip_preserves_identity() {
    let dim = 12;
    let (index, vectors) = build(dim);
    let dir = tempfile::tempdir().unwrap();
    index.save(dir.path()).unwrap();
    let loaded = PqrIndex::load(dir.path(), &[7u8; 32]).unwrap();
    assert_eq!(loaded.index_id(), index.index_id());
    let target = Target::new(5, 1.0, 0.01).unwrap();
    let opts = QueryOptions {
        target,
        nprobe: 2,
        reasons: Vec::new(),
        stage_eps: None,
    };
    let q = &vectors[..dim];
    let a = index.query(q, &opts).unwrap();
    let b = loaded.query(q, &opts).unwrap();
    match (a, b) {
        (QueryOutcome::Certified { hits: ha, .. }, QueryOutcome::Certified { hits: hb, .. }) => {
            assert_eq!(ha, hb)
        }
        _ => panic!("certified expected"),
    }
    assert!(matches!(
        PqrIndex::load(dir.path(), &[8u8; 32]),
        Err(IndexError::SeedMismatch)
    ));
}

#[test]
fn degraded_never_contains_certificate() {
    let dim = 12;
    let (index, vectors) = build(dim);
    let target = Target::new(5, 0.9, 0.01).unwrap();
    let outcome = index
        .query(
            &vectors[..dim],
            &QueryOptions {
                target,
                nprobe: 2,
                reasons: vec![DegradedReason::AdaptiveSequence],
                stage_eps: None,
            },
        )
        .unwrap();
    let QueryOutcome::Degraded { degraded, .. } = outcome else {
        panic!("expected degraded");
    };
    assert_eq!(degraded.status, pqr_core::certificate::Status::Degraded);
    assert!(matches!(
        index.query(
            &vectors[..dim],
            &QueryOptions {
                target: Target::new(2000, 1.0, 0.01).unwrap(),
                nprobe: 2,
                reasons: Vec::new(),
                stage_eps: None,
            },
        ),
        Err(IndexError::KExceedsLive)
    ));
}

#[test]
fn audit_is_falsification_only_and_reports_trials() {
    use pqr_core::audit::{audit, AuditConfig};
    let dim = 12;
    let (mut index, vectors) = build(dim);
    let queries: Vec<f32> = vectors[..dim * 20].to_vec();
    let target = Target::new(5, 1.0, 0.1).unwrap();
    let cfg = AuditConfig {
        rotations: 100,
        nprobe: 2,
        master_seed: [9u8; 32],
    };
    let original_id = index.index_id().to_string();
    let report = audit(&mut index, &queries, target, &cfg, None).unwrap();
    assert_eq!(report.index_id, original_id);
    assert_eq!(report.trials, 20 * 100);
    assert_eq!(report.inference, "falsification-only");
    assert_eq!(report.verdict, "pass");
    assert!(report.failure_rate <= target.delta);
}

#[test]
fn index_files_never_contain_the_seed() {
    use std::fs;
    let dim = 12;
    let vectors = data(600, dim);
    let seed = [0xABu8; 32];
    let (index, _) = PqrIndex::build(
        &vectors,
        dim,
        &BuildConfig {
            partitions: 12,
            stages: vec![6, 12],
            iterations: 5,
        },
        &seed,
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    index.save(dir.path()).unwrap();
    for entry in fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        let bytes = fs::read(&path).unwrap();
        assert!(
            !bytes.windows(seed.len()).any(|w| w == seed),
            "seed leaked into {}",
            path.display()
        );
        assert!(!path.to_string_lossy().contains("seed.bin"));
    }
}

#[test]
fn corrupt_index_is_rejected() {
    use std::fs;
    let dim = 12;
    let (index, _) = build(dim);
    let dir = tempfile::tempdir().unwrap();
    index.save(dir.path()).unwrap();
    let path = dir.path().join("codes.u64");
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 0xff;
    fs::write(&path, bytes).unwrap();
    assert!(matches!(
        PqrIndex::load(dir.path(), &[7u8; 32]),
        Err(IndexError::Checksum)
    ));
}
