use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use pqr_service::{build_router, AppState, Config, Error};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let config = Config::parse();
    let seed = config.seed()?;
    let state = AppState::load(&config, seed)?;
    let router = build_router(state.clone());
    let listener = TcpListener::bind(config.bind).await?;
    tracing::info!(bind = %config.bind, index = %config.index.display(), "pqr-serve started");
    spawn_reload_signal(state);
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| Error::Serve(error.to_string()))
}

#[cfg(unix)]
fn spawn_reload_signal(state: std::sync::Arc<AppState>) {
    use tokio::signal::unix::{signal, SignalKind};

    let mut hangup = match signal(SignalKind::hangup()) {
        Ok(signal) => signal,
        Err(error) => {
            tracing::warn!(error = %error, "SIGHUP reload disabled");
            return;
        }
    };
    tokio::spawn(async move {
        let mut last_reload = None;
        loop {
            hangup.recv().await;
            let now = tokio::time::Instant::now();
            if last_reload
                .is_some_and(|previous| now.duration_since(previous) < Duration::from_secs(1))
            {
                continue;
            }
            last_reload = Some(now);
            let worker = state.clone();
            match tokio::task::spawn_blocking(move || worker.reload_index()).await {
                Ok(Ok(())) => tracing::info!(event = "signal_reload", "index reloaded"),
                Ok(Err(pqr_service::Error::ReloadBusy)) => {
                    tracing::warn!(event = "signal_reload", "reload already in progress")
                }
                Ok(Err(error)) => {
                    tracing::warn!(event = "signal_reload", error = %error, "index reload failed")
                }
                Err(_) => tracing::warn!(event = "signal_reload", "reload task failed"),
            }
        }
    });
}

#[cfg(not(unix))]
fn spawn_reload_signal(_state: std::sync::Arc<AppState>) {}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut terminate = signal(SignalKind::terminate()).expect("SIGTERM listener");
    let mut interrupt = signal(SignalKind::interrupt()).expect("SIGINT listener");
    tokio::select! {
        _ = terminate.recv() => {}
        _ = interrupt.recv() => {}
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
