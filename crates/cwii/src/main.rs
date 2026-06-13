//! cwii — Cluster Workload Identity Injector webhook entrypoint.

mod config;
mod telemetry;
mod webhook;

use std::sync::Arc;

use anyhow::Context;
use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = config::Config::parse();
    let _telemetry = telemetry::init(cfg.otel_enabled, cfg.otel_endpoint.as_deref())
        .context("init telemetry")?;

    // Install the process-wide rustls crypto provider (kube/axum-server use the default provider,
    // aws-lc-rs, for their TLS).
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();

    let core = cfg.core();
    let providers = cfg.providers().context("build provider registry")?;
    let enabled: Vec<&str> = providers.iter().map(|p| p.id().abbr()).collect();
    tracing::info!(addr = %cfg.addr, providers = ?enabled, otel = cfg.otel_enabled, "starting cwii");

    let client = kube::Client::try_default()
        .await
        .context("build kubernetes client")?;

    let state = Arc::new(webhook::AppState {
        client,
        core,
        providers,
    });
    let app = webhook::router(state);

    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cfg.tls_cert, &cfg.tls_key)
        .await
        .with_context(|| format!("load TLS from {:?} / {:?}", cfg.tls_cert, cfg.tls_key))?;

    tracing::info!(addr = %cfg.addr, "listening");
    axum_server::bind_rustls(cfg.addr, tls)
        .serve(app.into_make_service())
        .await
        .context("serve")?;

    Ok(())
}
