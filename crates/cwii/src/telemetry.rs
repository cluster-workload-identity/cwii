//! Telemetry wiring: structured logs always, plus OpenTelemetry traces + metrics over OTLP when
//! enabled. Tracing spans are bridged to OTel via `tracing-opentelemetry`; metrics flow through the
//! global meter that [`cwii_core::telemetry`] records into. Export targets follow the standard
//! `OTEL_EXPORTER_OTLP_*` environment variables, with `--otel-endpoint` as a convenience override.

use anyhow::{Context, Result};
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

/// Shuts the OTel providers down (flushing pending spans/metrics) when dropped.
#[must_use = "hold the guard for the process lifetime so telemetry is flushed on shutdown"]
pub struct Guard {
    tracer: Option<SdkTracerProvider>,
    meter: Option<SdkMeterProvider>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(t) = &self.tracer {
            let _ = t.shutdown();
        }
        if let Some(m) = &self.meter {
            let _ = m.shutdown();
        }
    }
}

/// Initialise logging and, when `enabled`, OTLP traces + metrics. Returns a [`Guard`] that must be
/// kept alive for the process lifetime.
pub fn init(enabled: bool, endpoint: Option<&str>) -> Result<Guard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = tracing_subscriber::fmt::layer();

    if !enabled {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .init();
        return Ok(Guard {
            tracer: None,
            meter: None,
        });
    }

    let resource = Resource::builder().with_service_name("cwii").build();

    let span_exporter = {
        let b = opentelemetry_otlp::SpanExporter::builder().with_tonic();
        let b = match endpoint {
            Some(ep) => b.with_endpoint(ep),
            None => b,
        };
        b.build().context("build OTLP span exporter")?
    };
    let tracer_provider = SdkTracerProvider::builder()
        .with_resource(resource.clone())
        .with_batch_exporter(span_exporter)
        .build();
    global::set_tracer_provider(tracer_provider.clone());

    let metric_exporter = {
        let b = opentelemetry_otlp::MetricExporter::builder().with_tonic();
        let b = match endpoint {
            Some(ep) => b.with_endpoint(ep),
            None => b,
        };
        b.build().context("build OTLP metric exporter")?
    };
    let meter_provider = SdkMeterProvider::builder()
        .with_resource(resource)
        .with_periodic_exporter(metric_exporter)
        .build();
    global::set_meter_provider(meter_provider.clone());

    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer_provider.tracer("cwii"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();

    Ok(Guard {
        tracer: Some(tracer_provider),
        meter: Some(meter_provider),
    })
}
