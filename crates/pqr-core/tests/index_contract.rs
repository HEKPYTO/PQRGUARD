use pqr_core::certificate::{DegradedReason, Target};
use pqr_core::index::{
    BuildConfig, IndexError, PqrIndex, QueryOptions, QueryOutcome, MAX_INDEX_DIM,
    MAX_INDEX_FILE_BYTES, MAX_INDEX_N,
};

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
    all.sort_unstable_by(pqr_core::index::distance_then_id);
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
fn stage_eps_token_is_bound_to_its_index_and_target() {
    let dim = 12;
    let vectors = data(600, dim);
    let cfg = BuildConfig {
        partitions: 12,
        stages: vec![6, dim],
        iterations: 5,
    };
    let (index, _) = PqrIndex::build(&vectors, dim, &cfg, &[7u8; 32]).unwrap();
    let (other_index, _) = PqrIndex::build(&vectors, dim, &cfg, &[8u8; 32]).unwrap();
    let target = Target::new(5, 1.0, 0.01).unwrap();
    let token = index.stage_eps(&target).unwrap();
    let q = &vectors[..dim];
    let base = QueryOptions {
        target,
        nprobe: 2,
        reasons: Vec::new(),
        stage_eps: None,
    };
    let valid = QueryOptions {
        stage_eps: Some(token.clone()),
        ..base.clone()
    };
    assert!(matches!(
        index.query(q, &valid),
        Ok(QueryOutcome::Certified { .. })
    ));
    let foreign_target = QueryOptions {
        target: Target::new(5, 0.9, 0.01).unwrap(),
        stage_eps: Some(token.clone()),
        ..base.clone()
    };
    assert!(matches!(
        index.query(q, &foreign_target),
        Err(IndexError::StageEps)
    ));
    let foreign_index = QueryOptions {
        stage_eps: Some(token),
        ..base
    };
    assert!(matches!(
        other_index.query(q, &foreign_index),
        Err(IndexError::StageEps)
    ));
}

#[test]
fn audit_is_falsification_only_and_reports_trials() {
    use pqr_core::audit::{audit, AuditConfig};
    use pqr_core::seed::Seed;
    let dim = 12;
    let (mut index, vectors) = build(dim);
    let queries: Vec<f32> = vectors[..dim * 20].to_vec();
    let target = Target::new(5, 1.0, 0.1).unwrap();
    let cfg = AuditConfig {
        rotations: 100,
        nprobe: 2,
        master_seed: Seed::from_bytes([9u8; 32]),
    };
    let original_id = index.index_id().to_string();
    let report = audit(&mut index, &queries, target, &cfg, None).unwrap();
    assert_eq!(report.index_id, original_id);
    assert_ne!(index.index_id(), original_id);
    assert_eq!(report.trials, 20 * 100);
    assert_eq!(report.inference, "falsification-only");
    assert_eq!(report.verdict, "pass");
    assert!(report.failure_rate <= target.delta());
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

#[test]
fn build_and_query_reject_bad_inputs() {
    let cfg = BuildConfig {
        partitions: 2,
        stages: vec![4, 8],
        iterations: 1,
    };
    assert!(matches!(
        PqrIndex::build(&[], 8, &cfg, &[1u8; 32]),
        Err(IndexError::Vectors)
    ));
    let mut values = data(20, 8);
    values[0] = f32::NAN;
    assert!(matches!(
        PqrIndex::build(&values, 8, &cfg, &[1u8; 32]),
        Err(IndexError::NonFinite)
    ));
    let (index, vectors) = build(12);
    let target = Target::new(3, 1.0, 0.01).unwrap();
    let opts = QueryOptions {
        target,
        nprobe: 1,
        reasons: Vec::new(),
        stage_eps: None,
    };
    assert!(matches!(
        index.query(&[0.0; 11], &opts),
        Err(IndexError::Dimension)
    ));
    let mut q = vectors[..12].to_vec();
    q[0] = f32::NAN;
    assert!(matches!(index.query(&q, &opts), Err(IndexError::NonFinite)));
    assert!(matches!(
        index.query(
            &vectors[..12],
            &QueryOptions {
                target,
                nprobe: 0,
                reasons: Vec::new(),
                stage_eps: None,
            }
        ),
        Err(IndexError::Nprobe)
    ));
}

#[test]
fn out_of_distribution_queries_stay_correct() {
    let dim = 16;
    let (index, vectors) = build(dim);
    let target = Target::new(10, 1.0, 0.01).unwrap();
    let opts = QueryOptions {
        target,
        nprobe: 2,
        reasons: Vec::new(),
        stage_eps: None,
    };
    for offset in [20.0f32, 80.0, 200.0] {
        let q: Vec<f32> = (0..dim).map(|j| offset + j as f32 * 0.01).collect();
        let outcome = index.query(&q, &opts).unwrap();
        let QueryOutcome::Certified { hits, .. } = outcome else {
            panic!("expected certified");
        };
        let truth = brute(&vectors, dim, &q, 10);
        let got: Vec<u64> = hits.iter().map(|h| h.id).collect();
        assert_eq!(got, truth, "offset {offset}");
    }
}

#[test]
fn large_index_queries_cover_exhaustive_and_parallel_paths() {
    let dim = 8;
    let vectors = data(100_000, dim);
    let cfg = BuildConfig {
        partitions: 5,
        stages: vec![dim],
        iterations: 1,
    };
    let (index, _) = PqrIndex::build(&vectors, dim, &cfg, &[7u8; 32]).unwrap();
    let target = Target::new(1, 1.0, 0.01).unwrap();
    let q = &vectors[..dim];
    let options = |nprobe| QueryOptions {
        target,
        nprobe,
        reasons: Vec::new(),
        stage_eps: None,
    };
    let exhaustive = index.query(q, &options(8)).unwrap();
    let QueryOutcome::Certified { hits, .. } = exhaustive else {
        panic!("expected certified");
    };
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, 0);
    assert_eq!(hits[0].distance2, 0.0);
    let parallel = index.query(q, &options(1)).unwrap();
    let QueryOutcome::Certified { hits, .. } = parallel else {
        panic!("expected certified");
    };
    let got: Vec<u64> = hits.iter().map(|h| h.id).collect();
    assert_eq!(got, brute(&vectors, dim, q, 1));
}

#[test]
fn duplicate_distances_use_lowest_ids() {
    let dim = 4;
    let vectors = vec![0.0f32; 4 * 8];
    let (index, _) = PqrIndex::build(
        &vectors,
        dim,
        &BuildConfig {
            partitions: 2,
            stages: vec![2, 4],
            iterations: 2,
        },
        &[7u8; 32],
    )
    .unwrap();
    let target = Target::new(4, 1.0, 0.01).unwrap();
    let outcome = index
        .query(
            &[0.0; 4],
            &QueryOptions {
                target,
                nprobe: 2,
                reasons: Vec::new(),
                stage_eps: None,
            },
        )
        .unwrap();
    let QueryOutcome::Certified { hits, .. } = outcome else {
        panic!("expected certified");
    };
    let ids: Vec<u64> = hits.iter().map(|h| h.id).collect();
    assert_eq!(ids, vec![0, 1, 2, 3]);
    assert_eq!(brute(&vectors, dim, &[0.0; 4], 4), vec![0, 1, 2, 3]);
}

#[test]
fn atomic_save_replaces_index_without_mixed_files() {
    use std::fs;
    let dim = 12;
    let (first, vectors) = build(dim);
    let cfg = BuildConfig {
        partitions: 30,
        stages: vec![dim / 2, dim],
        iterations: 8,
    };
    let (second, _) = PqrIndex::build(&vectors, dim, &cfg, &[9u8; 32]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    first.save(dir.path()).unwrap();
    let first_id = first.index_id().to_string();

    fs::remove_file(dir.path().join("codes.u64")).unwrap();
    assert!(PqrIndex::load(dir.path(), &[7u8; 32]).is_err());

    second.save(dir.path()).unwrap();
    let loaded = PqrIndex::load(dir.path(), &[9u8; 32]).unwrap();
    assert_eq!(loaded.index_id(), second.index_id());
    assert_ne!(loaded.index_id(), first_id);
    assert!(matches!(
        PqrIndex::load(dir.path(), &[7u8; 32]),
        Err(IndexError::SeedMismatch)
    ));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 10);
}

#[cfg(unix)]
#[test]
fn failed_save_preserves_previous_index() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    let dim = 12;
    let (index, _) = build(dim);
    let outer = tempfile::tempdir().unwrap();
    let dir = outer.path().join("index");
    index.save(&dir).unwrap();
    let before = PqrIndex::load(&dir, &[7u8; 32]).unwrap();

    let mut permissions = fs::metadata(outer.path()).unwrap().permissions();
    permissions.set_mode(0o500);
    fs::set_permissions(outer.path(), permissions.clone()).unwrap();
    let result = index.save(&dir);
    let mut restored = permissions;
    restored.set_mode(0o700);
    fs::set_permissions(outer.path(), restored).unwrap();

    assert!(result.is_err());
    let after = PqrIndex::load(&dir, &[7u8; 32]).unwrap();
    assert_eq!(after.index_id(), before.index_id());
}

#[test]
fn index_load_rejects_oversized_manifest_fields() {
    use std::fs;
    let dim = 12;
    let (index, _) = build(dim);
    let dir = tempfile::tempdir().unwrap();
    index.save(dir.path()).unwrap();
    let path = dir.path().join("manifest.json");
    let original = fs::read(&path).unwrap();
    for (field, value) in [
        ("dim", MAX_INDEX_DIM + 1),
        ("n", MAX_INDEX_N + 1),
        ("partitions", MAX_INDEX_N + 1),
    ] {
        let mut manifest: serde_json::Value = serde_json::from_slice(&original).unwrap();
        manifest[field] = serde_json::json!(value);
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(
            matches!(
                PqrIndex::load(dir.path(), &[7u8; 32]),
                Err(IndexError::Limit(_))
            ),
            "{field} was accepted"
        );
    }
}

#[test]
fn index_load_checks_file_bytes_before_read() {
    use std::fs;
    let dim = 12;
    let (index, _) = build(dim);
    let dir = tempfile::tempdir().unwrap();
    index.save(dir.path()).unwrap();
    let path = dir.path().join("vectors.f32");
    fs::File::create(&path)
        .unwrap()
        .set_len(MAX_INDEX_FILE_BYTES + 1)
        .unwrap();
    assert!(matches!(
        PqrIndex::load(dir.path(), &[7u8; 32]),
        Err(IndexError::Limit(_))
    ));
}
