use std::fs;
use std::sync::Arc;
use std::time::Instant;

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use clap::Parser;
use pqr_core::index::{BuildConfig, PqrIndex};
use pqr_core::seed::Seed;
use pqr_service::{build_router, AppState, Config};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

fn seed_hex(seed: &[u8; 32]) -> String {
    seed.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn vectors(dim: usize, n: usize) -> Vec<f32> {
    (0..n)
        .flat_map(|row| (0..dim).map(move |column| (row * dim + column) as f32 * 0.125))
        .collect()
}

fn fixture() -> (TempDir, Router) {
    let dir = tempfile::tempdir().expect("fixture directory");
    let seed = [11u8; 32];
    let seed_file = dir.path().join("seed.hex");
    fs::write(&seed_file, seed_hex(&seed)).expect("seed file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&seed_file, fs::Permissions::from_mode(0o600))
            .expect("seed permissions");
    }
    let data = vectors(4, 16);
    let (index, _) = PqrIndex::build(
        &data,
        4,
        &BuildConfig {
            partitions: 4,
            stages: vec![2, 4],
            iterations: 2,
        },
        &seed,
    )
    .expect("build fixture index");
    index.save(dir.path()).expect("save fixture index");
    let args = vec![
        "pqr-serve".to_string(),
        "--index".to_string(),
        dir.path().to_str().expect("utf8 path").to_string(),
        "--seed-file".to_string(),
        seed_file.to_str().expect("utf8 path").to_string(),
        "--max-k".to_string(),
        "8".to_string(),
        "--max-nprobe".to_string(),
        "4".to_string(),
    ];
    let config = Config::parse_from(args);
    let state = Arc::new(AppState::new(index, config, Seed::from_bytes(seed)));
    (dir, build_router(state))
}

async fn query(router: Router) -> (StatusCode, Value) {
    let body =
        json!({"query": [0.25, 0.5, 0.75, 1.0], "k": 3, "tau": 1.0, "delta": 0.1, "nprobe": 2});
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/query")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = router.oneshot(request).await.expect("router response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
#[ignore]
async fn sixty_four_concurrent_certified_queries() {
    let (_dir, router) = fixture();
    const COUNT: usize = 64;
    let started = Instant::now();
    let mut tasks = Vec::with_capacity(COUNT);
    for _ in 0..COUNT {
        let router = router.clone();
        tasks.push(tokio::spawn(query(router)));
    }
    for task in tasks {
        let (status, body) = task.await.expect("query task");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "certified");
        assert_eq!(body["certificate"]["status"], "certified");
        assert!(body.get("degraded").is_none());
    }
    let elapsed = started.elapsed().as_secs_f64();
    eprintln!("throughput: {:.1} queries/s", COUNT as f64 / elapsed);
}
