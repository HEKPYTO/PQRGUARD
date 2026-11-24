use std::fs;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use clap::Parser;
use pqr_core::index::{BuildConfig, PqrIndex};
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
        "--max-k".to_string(),
        "8".to_string(),
        "--max-nprobe".to_string(),
        "4".to_string(),
    ];
    if let Some(token) = token {
        args.push("--api-token".to_string());
        args.push(token.to_string());
    }
    let config = Config::parse_from(args);
    (dir, Arc::new(AppState::new(index, config, seed)))
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
async fn valid_query_returns_certificate() {
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
async fn degraded_query_never_returns_certificate() {
    let (_dir, state) = fixture(None, 65_536);
    let router = build_router(state);
    let mut request = valid_query();
    request["public_seed"] = json!(true);
    let (status, body) = json_request(&router, "/v1/query", request.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "degraded");
    assert_eq!(body["degraded"]["status"], "degraded");
    assert_eq!(body["degraded"]["reasons"], json!(["public_seed"]));
    assert!(body.get("certificate").is_none());
    let (status, certified) = json_request(&router, "/v1/query", valid_query()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(certified["status"], "certified");
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
