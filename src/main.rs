mod config;
mod container_stats;
mod host;
mod models;
mod sink;
mod tasks;
mod web;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use bollard::Docker;
use tracing::info;

use crate::config::Config;
use crate::sink::{build_http_client, Sink};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Arc::new(Config::from_env());
    let http = Arc::new(build_http_client()?);

    let docker = Docker::connect_with_local_defaults()
        .context("Docker client (check DOCKER_HOST / socket)")?
        .negotiate_version()
        .await
        .context("Docker API version negotiation")?;

    info!("Waiting for Swarmbot…");
    let sink = Sink::new(config.clone(), (*http).clone());
    sink.wait_for_health().await;

    let sink = Arc::new(sink);

    tokio::spawn(tasks::events::run(docker.clone(), sink.clone()));
    info!("Event collector started.");

    tokio::spawn(tasks::stats::run(
        docker.clone(),
        sink.clone(),
        config.clone(),
    ));
    info!("Stats collector started.");

    let state = web::AppState {
        docker,
        config: config.clone(),
    };

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .context("bind :8080")?;
    info!("Swarmbot agent listening on port 8080");
    axum::serve(
        listener,
        web::router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("HTTP server")?;
    Ok(())
}
