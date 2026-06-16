//! Canonical `cwii.dev/*` annotation keys and the helpers that build per-provider keys.
//!
//! Provider-agnostic keys are `const`s; provider-scoped keys are built from
//! [`ProviderId::abbr`](crate::provider::ProviderId::abbr) so the set stays consistent as providers
//! are added.

use crate::provider::ProviderId;

/// Common prefix for every cwii annotation.
pub const PREFIX: &str = "cwii.dev";

/// Status annotation the webhook *writes* onto mutated pods: a comma-joined, sorted list of the
/// provider abbreviations that were injected (e.g. `gcp,aws`). Read back for idempotency.
pub const K_INJECTED: &str = "cwii.dev/injected";

/// `cwii.dev/<abbr>-inject` — enable injection for a provider (`"true"`/`"false"`).
pub fn inject_key(p: ProviderId) -> String {
    format!("{PREFIX}/{}-inject", p.abbr())
}

/// `cwii.dev/<abbr>-audience` — override the projected token audience for a provider.
pub fn audience_key(p: ProviderId) -> String {
    format!("{PREFIX}/{}-audience", p.abbr())
}

/// `cwii.dev/<abbr>-token-expiration` — projected token lifetime in seconds.
pub fn token_expiration_key(p: ProviderId) -> String {
    format!("{PREFIX}/{}-token-expiration", p.abbr())
}

/// `cwii.dev/<abbr>-verify` — add a non-blocking "can-i" verification init container.
pub fn verify_key(p: ProviderId) -> String {
    format!("{PREFIX}/{}-verify", p.abbr())
}

/// `cwii.dev/<abbr>-verify-enforce` — make a failed verification block pod startup.
pub fn verify_enforce_key(p: ProviderId) -> String {
    format!("{PREFIX}/{}-verify-enforce", p.abbr())
}

/// `cwii.dev/<abbr>-verify-image` — override the image used by the verification init container.
pub fn verify_image_key(p: ProviderId) -> String {
    format!("{PREFIX}/{}-verify-image", p.abbr())
}
