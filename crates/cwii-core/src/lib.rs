//! Provider-agnostic engine for **cwii**, the Cluster Workload Identity Injector.
//!
//! This crate knows nothing about any specific cloud. It defines the [`Provider`] trait that each
//! cloud implements, an intermediate representation ([`ProviderPlan`] / [`MutationPlan`])
//! describing *what* to inject into a pod, and the machinery that turns merged plans into
//! idempotent RFC 6902 JSON patches ([`patch::build`]) — plus the admission orchestration
//! ([`mutate`]) that ties it all together.
//!
//! Cloud behaviour lives in the `cwii-provider-*` crates, which depend on this one.

pub mod admission;
pub mod annotations;
pub mod config;
pub mod error;
pub mod k8s;
pub mod patch;
pub mod plan;
pub mod provider;
pub mod resolve;
pub mod telemetry;

pub use admission::{WebhookState, mutate};
pub use config::CoreConfig;
pub use error::Error;
pub use plan::{
    ConfigMapUpsert, EnvVar, InitContainer, MutationPlan, ProviderPlan, TokenSpec, VolumeMount,
    VolumeSpec, clamp_token_expiration, hardened_writer_security_context, merge, projected_token,
    verify_init_container, verify_security_context, writer_resources,
};
pub use provider::{Provider, ProviderContext, ProviderId};
pub use resolve::{AnnotationSet, enabled_with_native, native_or};
