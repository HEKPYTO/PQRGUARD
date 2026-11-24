use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, TryLockError};
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use pqr_core::certificate::{Certificate, Degraded, DegradedReason, Status, Target};
use pqr_core::index::{IndexError, PqrIndex, QueryOptions, QueryOutcome, StageEps};
use pqr_core::seed::{Seed, SeedError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tower::limit::ConcurrencyLimitLayer;

const ROUTES: [&str; 6] = [
    "/healthz",
    "/readyz",
    "/metrics",
    "/v1/query",
    "/admin/reload",
    "other",
];
const STATUS_CLASSES: [&str; 3] = ["2xx", "4xx", "5xx"];
const ERROR_REASONS: [&str; 4] = ["auth", "timeout", "client", "server"];
const MAX_CONCURRENCY_CAP: usize = 64;
const BUCKET_LIMITS: [&str; 8] = ["0.001", "0.005", "0.01", "0.05", "0.1", "0.5", "1", "+Inf"];
const BUCKET_MICROS: [u64; 7] = [1_000, 5_000, 10_000, 50_000, 100_000, 500_000, 1_000_000];
const MAX_EPS_ENTRIES: usize = 1024;

#[derive(Clone, PartialEq, Eq, Hash)]
struct EpsKey {
    index_id: String,
    k: usize,
    tau: u64,
    delta: u64,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("configuration: {0}")]
    Config(String),
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("seed: {0}")]
    Seed(#[from] SeedError),
    #[error("index: {0}")]
    Index(#[from] IndexError),
    #[error("reload already in progress")]
    ReloadBusy,
    #[error("server: {0}")]
    Serve(String),
}

#[derive(Clone, Parser)]
#[command(name = "pqr-serve", version, about = "HTTP frontend for pqr-guard")]
pub struct Config {
    #[arg(long, env = "PQR_INDEX", value_name = "DIR")]
    pub index: PathBuf,
    #[arg(long, env = "PQR_SEED_FILE", group = "seed", value_name = "PATH")]
    pub seed_file: Option<PathBuf>,
    #[arg(long, env = "PQR_SEED_ENV", group = "seed", value_name = "VAR")]
    pub seed_env: Option<String>,
    #[arg(long, env = "PQR_SEED_COMMAND", group = "seed", value_name = "CMD")]
    pub seed_command: Option<String>,
    #[arg(long, env = "PQR_BIND", default_value = "127.0.0.1:8080")]
    pub bind: SocketAddr,
    #[arg(long, env = "PQR_API_TOKEN")]
    pub api_token: Option<String>,
    #[arg(long, env = "PQR_MAX_BODY_BYTES", default_value_t = 65_536)]
    pub max_body_bytes: usize,
    #[arg(long, env = "PQR_QUERY_TIMEOUT_MS", default_value_t = 5_000)]
    pub query_timeout_ms: u64,
    #[arg(long, env = "PQR_MAX_K", default_value_t = 1_024)]
    pub max_k: usize,
    #[arg(long, env = "PQR_MAX_NPROBE", default_value_t = 64)]
    pub max_nprobe: usize,
    #[arg(long, env = "PQR_MAX_CONCURRENCY", default_value_t = 64)]
    pub max_concurrency: usize,
}

impl Config {
    pub fn validate(&self) -> Result<(), Error> {
        if self
            .api_token
            .as_deref()
            .is_some_and(|token| token.trim().is_empty())
        {
            return Err(Error::Config("api token must not be empty".to_string()));
        }
        if self.api_token.is_none() && !self.bind.ip().is_loopback() {
            return Err(Error::Config(
                "a bearer token is required for non-loopback binds".to_string(),
            ));
        }
        if self.max_body_bytes == 0
            || self.query_timeout_ms == 0
            || self.max_k == 0
            || self.max_nprobe == 0
            || self.max_concurrency == 0
        {
            return Err(Error::Config("limits must be positive".to_string()));
        }
        Ok(())
    }

    pub fn seed(&self) -> Result<Seed, Error> {
        if let Some(path) = &self.seed_file {
            return Ok(Seed::from_file(path)?);
        }
        if let Some(name) = &self.seed_env {
            return Ok(Seed::from_env(name)?);
        }
        if let Some(command) = &self.seed_command {
            return Ok(Seed::from_command(command)?);
        }
        Err(Error::Config(
            "one of --seed-file, --seed-env, or --seed-command is required".to_string(),
        ))
    }
}

pub struct Metrics {
    requests: [[AtomicU64; 3]; 6],
    errors: [AtomicU64; 4],
    buckets: [AtomicU64; 8],
    sum_micros: AtomicU64,
    count: AtomicU64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            requests: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
            errors: std::array::from_fn(|_| AtomicU64::new(0)),
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            sum_micros: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }
}

impl Metrics {
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(4_096);
        out.push_str("# HELP pqr_http_requests_total HTTP requests by route and status class.\n");
        out.push_str("# TYPE pqr_http_requests_total counter\n");
        for (route_index, route) in ROUTES.iter().enumerate() {
            for (class_index, class) in STATUS_CLASSES.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "pqr_http_requests_total{{route=\"{route}\",code=\"{class}\"}} {}",
                    self.requests[route_index][class_index].load(Ordering::Relaxed)
                );
            }
        }
        out.push_str("# HELP pqr_http_errors_total HTTP errors by reason.\n");
        out.push_str("# TYPE pqr_http_errors_total counter\n");
        for (index, reason) in ERROR_REASONS.iter().enumerate() {
            let _ = writeln!(
                out,
                "pqr_http_errors_total{{reason=\"{reason}\"}} {}",
                self.errors[index].load(Ordering::Relaxed)
            );
        }
        out.push_str("# HELP pqr_query_duration_seconds Query latency in seconds.\n");
        out.push_str("# TYPE pqr_query_duration_seconds histogram\n");
        for (index, limit) in BUCKET_LIMITS.iter().enumerate() {
            let _ = writeln!(
                out,
                "pqr_query_duration_seconds_bucket{{le=\"{limit}\"}} {}",
                self.buckets[index].load(Ordering::Relaxed)
            );
        }
        let _ = writeln!(
            out,
            "pqr_query_duration_seconds_sum {}",
            self.sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0
        );
        let _ = writeln!(
            out,
            "pqr_query_duration_seconds_count {}",
            self.count.load(Ordering::Relaxed)
        );
        out
    }

    fn observe_request(&self, path: &str, status: StatusCode) {
        let route = ROUTES.iter().position(|item| *item == path).unwrap_or(5);
        let class = match status.as_u16() {
            200..=299 => 0,
            400..=499 => 1,
            _ => 2,
        };
        self.requests[route][class].fetch_add(1, Ordering::Relaxed);
        let reason = match status.as_u16() {
            401 | 403 => 0,
            504 => 1,
            400..=499 => 2,
            _ => 3,
        };
        if !status.is_success() {
            self.errors[reason].fetch_add(1, Ordering::Relaxed);
        }
    }

    fn observe_latency(&self, elapsed: Duration) {
        let micros = elapsed.as_micros().min(u64::MAX as u128) as u64;
        for (index, limit) in BUCKET_MICROS.iter().enumerate() {
            if micros <= *limit {
                self.buckets[index].fetch_add(1, Ordering::Relaxed);
            }
        }
        self.buckets[7].fetch_add(1, Ordering::Relaxed);
        self.sum_micros.fetch_add(micros, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }
}

pub struct AppState {
    index: RwLock<Arc<PqrIndex>>,
    ready: AtomicBool,
    seed: Seed,
    config: Config,
    metrics: Arc<Metrics>,
    eps_cache: Mutex<HashMap<EpsKey, StageEps>>,
    reload_lock: Mutex<()>,
}

impl AppState {
    pub fn new(index: PqrIndex, config: Config, seed: Seed) -> Self {
        Self {
            index: RwLock::new(Arc::new(index)),
            ready: AtomicBool::new(true),
            seed,
            config,
            eps_cache: Mutex::new(HashMap::new()),
            metrics: Arc::new(Metrics::default()),
            reload_lock: Mutex::new(()),
        }
    }

    pub fn load(config: &Config, seed: Seed) -> Result<Arc<Self>, Error> {
        config.validate()?;
        let index = PqrIndex::load(&config.index, seed.as_bytes())?;
        Ok(Arc::new(Self::new(index, config.clone(), seed)))
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn reload_index(&self) -> Result<(), Error> {
        let guard = match self.reload_lock.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(Error::ReloadBusy),
        };
        self.ready.store(false, Ordering::Release);
        let index = PqrIndex::load(&self.config.index, self.seed.as_bytes())?;
        let mut current = self
            .index
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *current = Arc::new(index);
        self.ready.store(true, Ordering::Release);
        drop(guard);
        Ok(())
    }

    fn stage_eps(&self, index: &PqrIndex, target: &Target) -> Result<StageEps, IndexError> {
        let key = EpsKey {
            index_id: index.index_id().to_string(),
            k: target.k(),
            tau: target.tau().to_bits(),
            delta: target.delta().to_bits(),
        };
        if let Some(token) = self
            .eps_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            return Ok(token.clone());
        }
        let token = index.stage_eps(target)?;
        let mut cache = self
            .eps_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cache.len() >= MAX_EPS_ENTRIES {
            cache.clear();
        }
        cache.insert(key, token.clone());
        Ok(token)
    }

    fn live_index(&self) -> Option<Arc<PqrIndex>> {
        if self.is_ready() {
            Some(
                self.index
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone(),
            )
        } else {
            None
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRequest {
    pub query: Vec<f32>,
    pub k: usize,
    pub tau: f64,
    pub delta: f64,
    pub nprobe: usize,
    #[serde(default)]
    pub public_seed: bool,
}

#[derive(Debug, Serialize)]
pub struct QueryResponse {
    pub hits: Vec<pqr_core::index::Hit>,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate: Option<Certificate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<Degraded>,
}

struct ApiError {
    status: StatusCode,
    message: &'static str,
}

impl ApiError {
    fn new(status: StatusCode, message: &'static str) -> Self {
        Self { status, message }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

fn token_matches(expected: &[u8], actual: &[u8]) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in expected.iter().zip(actual) {
        difference |= left ^ right;
    }
    difference == 0
}

pub fn build_router(state: Arc<AppState>) -> Router {
    let protected = Router::new()
        .route("/v1/query", post(query_handler))
        .route("/admin/reload", post(reload_handler))
        .route("/metrics", get(metrics_handler))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));
    let open = Router::new()
        .route("/healthz", get(health_handler))
        .route("/readyz", get(ready_handler));
    open.merge(protected)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            observe_middleware,
        ))
        .layer(ConcurrencyLimitLayer::new(
            state.config.max_concurrency.min(MAX_CONCURRENCY_CAP),
        ))
        .layer(DefaultBodyLimit::max(state.config.max_body_bytes))
        .with_state(state)
}

async fn health_handler() -> &'static str {
    "ok\n"
}

async fn ready_handler(State(state): State<Arc<AppState>>) -> Response {
    if state.is_ready() {
        (StatusCode::OK, "ready\n").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready\n").into_response()
    }
}

async fn metrics_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        state.metrics.render(),
    )
}

async fn auth_middleware(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let Some(expected) = state.config.api_token.as_deref() else {
        return next.run(req).await;
    };
    let authorized = !expected.trim().is_empty()
        && req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|provided| token_matches(expected.as_bytes(), provided.as_bytes()));
    if !authorized {
        return ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    next.run(req).await
}

async fn observe_middleware(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let started = Instant::now();
    let response = next.run(req).await;
    let status = response.status();
    state.metrics.observe_request(&path, status);
    if path == "/v1/query" {
        state.metrics.observe_latency(started.elapsed());
    }
    tracing::info!(
        method = %method,
        path = %path,
        status = status.as_u16(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "request"
    );
    response
}

async fn query_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    let invalid = || ApiError::new(StatusCode::BAD_REQUEST, "invalid query");
    let index = state
        .live_index()
        .ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "index unavailable"))?;
    if request.k == 0
        || request.k > state.config.max_k
        || request.k > index.len()
        || request.nprobe == 0
        || request.nprobe > state.config.max_nprobe
        || request.query.is_empty()
        || request.query.len() != index.dim()
        || request.query.iter().any(|value| !value.is_finite())
    {
        return Err(invalid());
    }
    let target = Target::new(request.k, request.tau, request.delta).map_err(|_| invalid())?;
    let reasons = if request.public_seed {
        vec![DegradedReason::PublicSeed]
    } else {
        Vec::new()
    };
    let nprobe = request.nprobe;
    let query = request.query;
    let worker = state.clone();
    let timeout = Duration::from_millis(state.config.query_timeout_ms);
    let outcome = tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || {
            let stage_eps = worker.stage_eps(&index, &target)?;
            let options = QueryOptions {
                target,
                nprobe,
                reasons,
                stage_eps: Some(stage_eps),
            };
            index.query(&query, &options)
        }),
    )
    .await;
    let outcome = match outcome {
        Ok(Ok(Ok(outcome))) => outcome,
        Ok(Ok(Err(error))) => {
            return Err(match error {
                IndexError::Dimension
                | IndexError::Vectors
                | IndexError::NonFinite
                | IndexError::KExceedsLive
                | IndexError::Nprobe
                | IndexError::Stages
                | IndexError::Math(_) => invalid(),
                IndexError::StageEps => {
                    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "query failed")
                }
                _ => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "query failed"),
            })
        }
        Ok(Err(_)) => {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "query failed",
            ))
        }
        Err(_) => {
            return Err(ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "query timed out",
            ))
        }
    };
    let response = match outcome {
        QueryOutcome::Certified { hits, certificate } => QueryResponse {
            hits,
            status: Status::Certified,
            certificate: Some(certificate),
            degraded: None,
        },
        QueryOutcome::Degraded { hits, degraded } => QueryResponse {
            hits,
            status: Status::Degraded,
            certificate: None,
            degraded: Some(degraded),
        },
    };
    Ok(Json(response))
}

async fn reload_handler(State(state): State<Arc<AppState>>) -> Response {
    let result = tokio::task::spawn_blocking(move || state.reload_index()).await;
    match result {
        Ok(Ok(())) => Json(json!({ "status": "ok" })).into_response(),
        Ok(Err(Error::ReloadBusy)) => {
            ApiError::new(StatusCode::CONFLICT, "reload already in progress").into_response()
        }
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "index reload rejected");
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "index reload failed").into_response()
        }
        Err(_) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "index reload failed").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use pqr_core::index::BuildConfig;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use tower::ServiceExt;

    fn test_state(query_timeout_ms: u64) -> (tempfile::TempDir, Arc<AppState>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let seed = Seed::from_bytes([11u8; 32]);
        let data: Vec<f32> = (0..64)
            .flat_map(|row| (0..4).map(move |column| (row * 4 + column) as f32 * 0.125))
            .collect();
        let (index, _) = PqrIndex::build(
            &data,
            4,
            &BuildConfig {
                partitions: 4,
                stages: vec![2, 4],
                iterations: 2,
            },
            seed.as_bytes(),
        )
        .expect("index");
        index.save(dir.path()).expect("save index");
        let config = Config {
            index: dir.path().to_path_buf(),
            seed_file: None,
            seed_env: Some("PQR_TEST_SEED".to_string()),
            seed_command: None,
            bind: "127.0.0.1:0".parse().expect("bind"),
            api_token: None,
            max_body_bytes: 65_536,
            query_timeout_ms,
            max_k: 8,
            max_nprobe: 4,
            max_concurrency: 64,
        };
        (dir, Arc::new(AppState::new(index, config, seed)))
    }

    #[test]
    fn poisoned_locks_are_recovered() {
        let (_dir, state) = test_state(5_000);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = state.index.write().expect("index lock");
            panic!("poison index");
        }));
        assert!(state.live_index().is_some());
        assert!(state.reload_index().is_ok());
        assert!(state.is_ready());

        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = state.eps_cache.lock().expect("eps lock");
            panic!("poison eps");
        }));
        let target = Target::new(3, 1.0, 0.1).expect("target");
        let index = state.live_index().expect("index");
        assert!(state.stage_eps(&index, &target).is_ok());

        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = state.reload_lock.lock().expect("reload lock");
            panic!("poison reload");
        }));
        assert!(state.reload_index().is_ok());
        assert!(state.is_ready());
    }

    #[test]
    fn overlapping_reload_returns_busy() {
        let (_dir, state) = test_state(5_000);
        let guard = state.reload_lock.lock().expect("reload lock");
        assert!(matches!(state.reload_index(), Err(Error::ReloadBusy)));
        assert!(state.is_ready());
        drop(guard);
        assert!(state.reload_index().is_ok());
    }

    #[tokio::test]
    async fn mismatched_stage_eps_is_server_error() {
        let (_dir, state) = test_state(5_000);
        let target = Target::new(3, 1.0, 0.1).expect("target");
        let data: Vec<f32> = (0..80).map(|value| value as f32 * 0.125).collect();
        let (other, _) = PqrIndex::build(
            &data,
            4,
            &BuildConfig {
                partitions: 4,
                stages: vec![2, 4],
                iterations: 2,
            },
            &[11u8; 32],
        )
        .expect("other index");
        let token = other.stage_eps(&target).expect("token");
        let key = EpsKey {
            index_id: state.index.read().expect("index").index_id().to_string(),
            k: target.k(),
            tau: target.tau().to_bits(),
            delta: target.delta().to_bits(),
        };
        state.eps_cache.lock().expect("cache").insert(key, token);
        let router = build_router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/query")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "query": [0.25, 0.5, 0.75, 1.0],
                    "k": 3,
                    "tau": 1.0,
                    "delta": 0.1,
                    "nprobe": 2
                })
                .to_string(),
            ))
            .expect("request");
        let response = router.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("error body");
        assert!(String::from_utf8(bytes.to_vec())
            .expect("utf8")
            .contains("query failed"));
        let request = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .expect("request");
        let response = router.oneshot(request).await.expect("metrics");
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("metrics body");
        let metrics = String::from_utf8(bytes.to_vec()).expect("metrics utf8");
        assert!(
            metrics.contains("pqr_http_errors_total{reason=\"server\"} 1"),
            "{metrics}"
        );
    }
    #[tokio::test]
    async fn blocked_worker_times_out_and_counts_timeout() {
        let (_dir, state) = test_state(1);
        let router = build_router(state.clone());
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = state.clone();
        let blocker = std::thread::spawn(move || {
            let _guard = worker.eps_cache.lock().expect("cache");
            locked_tx.send(()).expect("lock signal");
            release_rx.recv().expect("release signal");
        });
        locked_rx.recv().expect("cache locked");
        let request = Request::builder()
            .method("POST")
            .uri("/v1/query")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "query": [0.25, 0.5, 0.75, 1.0],
                    "k": 3,
                    "tau": 1.0,
                    "delta": 0.1,
                    "nprobe": 2
                })
                .to_string(),
            ))
            .expect("request");
        let response = router.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        release_tx.send(()).expect("release");
        blocker.join().expect("blocker");
        let request = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .expect("request");
        let response = router.oneshot(request).await.expect("metrics");
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("metrics body");
        let metrics = String::from_utf8(bytes.to_vec()).expect("metrics utf8");
        assert!(
            metrics.contains("pqr_http_errors_total{reason=\"timeout\"} 1"),
            "{metrics}"
        );
    }
}
