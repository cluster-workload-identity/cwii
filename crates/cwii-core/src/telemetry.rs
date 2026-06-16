//! OpenTelemetry metric instruments, created lazily from the global meter.
//!
//! The binary installs the meter provider at startup; this module records into it. When telemetry
//! is disabled the global meter is a no-op, so recording is effectively free and callers never need
//! to branch on whether telemetry is enabled.

use std::sync::OnceLock;

use opentelemetry::global;
use opentelemetry::metrics::{Counter, Histogram};

/// cwii's metric instruments.
pub struct Metrics {
    /// Admission requests handled, attribute `outcome` = skip | inject | deny | patch_error.
    pub requests: Counter<u64>,
    /// Provider injections planned, attribute `provider` = gcp | aws | az.
    pub injections: Counter<u64>,
    /// Mutation handling duration, in seconds.
    pub duration: Histogram<f64>,
}

static METRICS: OnceLock<Metrics> = OnceLock::new();

/// The process-wide metric instruments.
pub fn metrics() -> &'static Metrics {
    METRICS.get_or_init(|| {
        let meter = global::meter("cwii");
        Metrics {
            requests: meter
                .u64_counter("cwii.admission.requests")
                .with_description("AdmissionReview requests handled, by outcome")
                .build(),
            injections: meter
                .u64_counter("cwii.injections")
                .with_description("Provider injections planned, by provider")
                .build(),
            duration: meter
                .f64_histogram("cwii.admission.duration")
                .with_unit("s")
                .with_description("Time spent handling a mutation request")
                .build(),
        }
    })
}
