use std::fs;
use std::io;
use std::sync::Arc;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use axum::body::{to_bytes, Body, Bytes};
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

fn fixture(token: Option<&str>, max_body_bytes: usize) -> (TempDir, Arc<AppState>) {
    fixture_with(token, max_body_bytes, 64, 5_000, false)
}

fn fixture_with(
    token: Option<&str>,
    max_body_bytes: usize,
    max_concurrency: usize,
    query_timeout_ms: u64,
    seed_public: bool,
) -> (TempDir, Arc<AppState>) {
    let dir = tempfile::tempdir().expect("fixture directory");
    let seed = [11u8; 32];
    let seed_file = dir.path().join("seed.hex");
    fs::write(&seed_file, seed_hex(&seed)).expect("seed file");
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
    let mut args = vec![
        "pqr-serve".to_string(),
        "--index".to_string(),
        dir.path().to_str().expect("utf8 path").to_string(),
        "--seed-file".to_string(),
        seed_file.to_str().expect("utf8 path").to_string(),
        "--max-body-bytes".to_string(),
        max_body_bytes.to_string(),
        "--max-concurrency".to_string(),
        max_concurrency.to_string(),
        "--query-timeout-ms".to_string(),
        query_timeout_ms.to_string(),
        "--max-k".to_string(),
        "8".to_string(),
        "--max-nprobe".to_string(),
        "4".to_string(),
    ];
    if let Some(token) = token {
        args.push("--api-token".to_string());
        args.push(token.to_string());
    }
    if seed_public {
        args.push("--seed-public".to_string());
    }
    let config = Config::parse_from(args);
    (
        dir,
        Arc::new(AppState::new(index, config, Seed::from_bytes(seed))),
    )
}

fn valid_query() -> Value {
    json!({"query": [0.25, 0.5, 0.75, 1.0], "k": 3, "tau": 1.0, "delta": 0.1, "nprobe": 2})
}

async fn send(router: &Router, method: Method, uri: &str, body: Body) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .expect("request");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    (
        status,
        String::from_utf8(bytes.to_vec()).expect("utf8 body"),
    )
}

async fn json_request(router: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let (status, text) = send(
        router,
        Method::POST,
        uri,
        Body::from(serde_json::to_string(&body).expect("json body")),
    )
    .await;
    let value = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, value)
}

async fn send_with_token(
    router: &Router,
    method: Method,
    uri: &str,
    token: &str,
    body: Body,
) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(body)
        .expect("request");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    (
        status,
        String::from_utf8(bytes.to_vec()).expect("utf8 body"),
    )
}

#[tokio::test]
async fn health_ready_and_metrics_exposed() {
    let (_dir, state) = fixture(None, 65_536);
    let router = build_router(state);
    let (status, body) = send(&router, Method::GET, "/healthz", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok\n");
    let (status, body) = send(&router, Method::GET, "/readyz", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ready\n");
    let (status, body) = send(&router, Method::GET, "/metrics", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("pqr_http_requests_total{route=\"/healthz\",code=\"2xx\"} 1"));
    assert!(body.contains("pqr_query_duration_seconds_bucket{le=\"+Inf\"} 0"));
    assert!(body.contains("pqr_http_errors_total{reason=\"client\"} 0"));
}

#[tokio::test]
async fn bearer_token_required_for_query_metrics_and_reload() {
    let (_dir, state) = fixture(Some("s3cret-token"), 65_536);
    let router = build_router(state);
    let (status, _) = json_request(&router, "/v1/query", valid_query()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = json_request(
        &router,
        "/v1/query",
        json!({"query": [0.0], "k": 1, "tau": 1.0, "delta": 0.1, "nprobe": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&router, Method::GET, "/metrics", Body::empty()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&router, Method::POST, "/admin/reload", Body::empty()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&router, Method::GET, "/healthz", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);

    let authorized = Request::builder()
        .method(Method::POST)
        .uri("/v1/query")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer s3cret-token")
        .body(Body::from(valid_query().to_string()))
        .expect("request");
    let response = router
        .clone()
        .oneshot(authorized)
        .await
        .expect("authorized response");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn default_config_returns_certificate() {
    let (_dir, state) = fixture(None, 65_536);
    let router = build_router(state);
    let (status, body) = json_request(&router, "/v1/query", valid_query()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "certified");
    assert_eq!(body["certificate"]["status"], "certified");
    assert_eq!(body["certificate"]["k"], 3);
    assert_eq!(body["hits"].as_array().expect("hits").len(), 3);
    assert!(body.get("degraded").is_none());
    let (_, metrics) = send(&router, Method::GET, "/metrics", Body::empty()).await;
    assert!(metrics.contains("pqr_query_duration_seconds_count 1"));
}

#[tokio::test]
async fn configured_public_seed_degrades_without_certificate() {
    let (_dir, state) = fixture_with(None, 65_536, 64, 5_000, true);
    let router = build_router(state);
    let (status, body) = json_request(&router, "/v1/query", valid_query()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "degraded");
    assert_eq!(body["degraded"]["status"], "degraded");
    assert_eq!(body["degraded"]["reasons"], json!(["public_seed"]));
    assert!(body.get("certificate").is_none());
}

#[tokio::test]
async fn client_cannot_set_public_seed() {
    let (_dir, state) = fixture(None, 65_536);
    let router = build_router(state);
    let mut request = valid_query();
    request["public_seed"] = json!(true);
    let (status, text) = send(
        &router,
        Method::POST,
        "/v1/query",
        Body::from(request.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(text.contains("public_seed"), "{text}");
}

#[tokio::test]
async fn invalid_queries_rejected() {
    let (_dir, state) = fixture(None, 65_536);
    let router = build_router(state);
    let cases = vec![
        json!({"query": [0.0, 0.0, 0.0, 0.0], "k": 0, "tau": 1.0, "delta": 0.1, "nprobe": 1}),
        json!({"query": [0.0, 0.0, 0.0, 0.0], "k": 9, "tau": 1.0, "delta": 0.1, "nprobe": 1}),
        json!({"query": [0.0, 0.0, 0.0, 0.0], "k": 3, "tau": 0.0, "delta": 0.1, "nprobe": 1}),
        json!({"query": [0.0, 0.0, 0.0, 0.0], "k": 3, "tau": 1.0, "delta": 1.0, "nprobe": 1}),
        json!({"query": [0.0, 0.0, 0.0, 0.0], "k": 3, "tau": 1.0, "delta": 0.1, "nprobe": 0}),
        json!({"query": [0.0, 0.0, 0.0, 0.0], "k": 3, "tau": 1.0, "delta": 0.1, "nprobe": 5}),
        json!({"query": [0.0, 0.0, 0.0], "k": 3, "tau": 1.0, "delta": 0.1, "nprobe": 1}),
        json!({"query": [0.0, null, 0.0, 0.0], "k": 3, "tau": 1.0, "delta": 0.1, "nprobe": 1}),
        json!({"query": [0.0, 0.0, 0.0, 0.0], "k": 3, "tau": 1.0, "delta": 0.1, "nprobe": 1, "extra": true}),
    ];
    for case in cases {
        let (status, body) = json_request(&router, "/v1/query", case).await;
        assert!(
            matches!(
                status,
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
            ),
            "case body: {body}"
        );
        assert!(body.get("certificate").is_none());
    }
    let (status, body) = send(
        &router,
        Method::POST,
        "/v1/query",
        Body::from("{\"query\": ["),
    )
    .await;
    assert!(
        matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ),
        "raw body: {body}"
    );
    let (status, body) = send(
        &router,
        Method::POST,
        "/v1/query",
        Body::from(
            "{\"query\":[0.0,0.0,0.0,1e400],\"k\":3,\"tau\":1.0,\"delta\":0.1,\"nprobe\":1}",
        ),
    )
    .await;
    assert!(
        matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ),
        "raw body: {body}"
    );
}

#[tokio::test]
async fn oversized_body_rejected() {
    let (_dir, state) = fixture(None, 128);
    let router = build_router(state);
    let huge = json!({
        "query": vec![0.0f64; 128],
        "k": 3,
        "tau": 1.0,
        "delta": 0.1,
        "nprobe": 1
    });
    let (status, _) = json_request(&router, "/v1/query", huge).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn failed_reload_marks_not_ready_and_success_restores() {
    let (dir, state) = fixture(None, 65_536);
    let router = build_router(state);
    let manifest = dir.path().join("manifest.json");
    let backup = fs::read(&manifest).expect("manifest backup");
    fs::write(&manifest, b"{}").expect("corrupt manifest");

    let (status, _) = send(&router, Method::POST, "/admin/reload", Body::empty()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = send(&router, Method::GET, "/readyz", Body::empty()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, body) = json_request(&router, "/v1/query", valid_query()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.get("certificate").is_none());

    fs::write(&manifest, backup).expect("restore manifest");
    let (status, _) = send(&router, Method::POST, "/admin/reload", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&router, Method::GET, "/readyz", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = json_request(&router, "/v1/query", valid_query()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["certificate"]["status"], "certified");
}

#[tokio::test]
async fn non_loopback_bind_requires_token() {
    let (_dir, state) = fixture(None, 65_536);
    let config = Config::parse_from([
        "pqr-serve",
        "--index",
        "/tmp/index",
        "--seed-env",
        "PQR_TEST_SEED",
        "--bind",
        "0.0.0.0:8080",
    ]);
    assert!(config.validate().is_err());
    let token_config = Config::parse_from([
        "pqr-serve",
        "--index",
        "/tmp/index",
        "--seed-env",
        "PQR_TEST_SEED",
        "--bind",
        "0.0.0.0:8080",
        "--api-token",
        "token",
    ]);
    assert!(token_config.validate().is_ok());
    assert!(state.is_ready());
}

#[tokio::test]
async fn empty_api_token_rejected() {
    for token in ["", "   "] {
        let config = Config::parse_from([
            "pqr-serve",
            "--index",
            "/tmp/index",
            "--seed-env",
            "PQR_TEST_SEED",
            "--api-token",
            token,
        ]);
        assert!(config.validate().is_err(), "token {token:?}");
    }
    let (_dir, state) = fixture(Some(""), 65_536);
    let router = build_router(state);
    let (status, _) = json_request(&router, "/v1/query", valid_query()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_same_length_token_is_rejected_and_counted() {
    let (_dir, state) = fixture(Some("s3cret-token"), 65_536);
    let router = build_router(state);
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/query")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer s3cret-tokeX")
        .body(Body::from(valid_query().to_string()))
        .expect("request");
    let response = router.clone().oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let (status, metrics) = send_with_token(
        &router,
        Method::GET,
        "/metrics",
        "s3cret-token",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        metrics.contains("pqr_http_errors_total{reason=\"auth\"} 1"),
        "{metrics}"
    );
}

#[tokio::test]
async fn concurrency_limit_holds_permit_until_body_finishes() {
    let (_dir, state) = fixture_with(None, 65_536, 1, 5_000, false);
    let router = build_router(state);
    let (tx, rx) = tokio::sync::oneshot::channel::<Bytes>();
    let held_router = router.clone();
    let held = tokio::spawn(async move {
        let stream = futures_util::stream::once(async move {
            rx.await
                .map(Ok)
                .unwrap_or_else(|_| Err(io::Error::other("body closed")))
        });
        send(
            &held_router,
            Method::POST,
            "/v1/query",
            Body::from_stream(stream),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let second_router = router.clone();
    let mut second =
        tokio::spawn(async move { json_request(&second_router, "/v1/query", valid_query()).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut second)
            .await
            .is_err(),
        "second request completed while permit was held"
    );
    drop(tx);
    let (status, _) = held.await.expect("held request");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = second.await.expect("second request");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["certificate"]["status"], "certified");
}

#[tokio::test]
async fn unknown_paths_share_one_metric_label() {
    let (_dir, state) = fixture(None, 65_536);
    let router = build_router(state);
    for path in ["/nope", "/nope/1", "/v1/nope"] {
        let (status, _) = send(&router, Method::GET, path, Body::empty()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let (_, metrics) = send(&router, Method::GET, "/metrics", Body::empty()).await;
    let line = metrics
        .lines()
        .find(|line| line.starts_with("pqr_http_requests_total{route=\"other\",code=\"4xx\"}"))
        .expect("other route metric");
    assert_eq!(
        line,
        "pqr_http_requests_total{route=\"other\",code=\"4xx\"} 3"
    );
    assert!(!metrics.contains("route=\"/nope\""));
}

#[test]
fn seed_file_accepts_trailing_newline() {
    let dir = tempfile::tempdir().expect("temp dir");
    let seed = [11u8; 32];
    let raw = seed.iter().copied().chain(std::iter::once(b'\n')).collect();
    for (name, bytes) in [
        ("seed.hex", format!("{}\n", seed_hex(&seed)).into_bytes()),
        ("seed.raw", raw),
    ] {
        let path = dir.path().join(name);
        fs::write(&path, bytes).expect("seed file");
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("seed permissions");
        let config = Config::parse_from([
            "pqr-serve",
            "--index",
            "/tmp/index",
            "--seed-file",
            path.to_str().expect("utf8 path"),
        ]);
        assert_eq!(config.seed().expect("seed").as_bytes(), &seed);
    }
}

#[cfg(unix)]
#[test]
fn seed_file_rejects_world_readable_permissions() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("seed.hex");
    fs::write(&path, format!("{}\n", seed_hex(&[11u8; 32]))).expect("seed file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("seed permissions");
    let config = Config::parse_from([
        "pqr-serve",
        "--index",
        "/tmp/index",
        "--seed-file",
        path.to_str().expect("utf8 path"),
    ]);
    assert!(config.seed().is_err());
}

#[tokio::test]
async fn concurrent_reloads_keep_ready_and_latest_index() {
    let (dir, state) = fixture(None, 65_536);
    let router = build_router(state);
    let seed = [11u8; 32];
    let (replacement, _) = PqrIndex::build(
        &vectors(4, 20),
        4,
        &BuildConfig {
            partitions: 4,
            stages: vec![2, 4],
            iterations: 2,
        },
        &seed,
    )
    .expect("replacement index");
    for entry in fs::read_dir(dir.path()).expect("index dir") {
        let entry = entry.expect("entry");
        if entry.path().is_file() {
            fs::remove_file(entry.path()).expect("remove old index file");
        }
    }
    replacement.save(dir.path()).expect("save replacement");
    let expected_id = replacement.index_id().to_string();
    let (first, second) = tokio::join!(
        send(&router, Method::POST, "/admin/reload", Body::empty()),
        send(&router, Method::POST, "/admin/reload", Body::empty()),
    );
    for (status, _) in [first, second] {
        assert!(matches!(status, StatusCode::OK | StatusCode::CONFLICT));
    }
    let (status, _) = send(&router, Method::GET, "/readyz", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = json_request(&router, "/v1/query", valid_query()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["certificate"]["index_id"], expected_id);
}
