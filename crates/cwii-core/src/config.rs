//! Shared, provider-agnostic configuration handed to providers via
//! [`ProviderContext`](crate::provider::ProviderContext). Provider-specific configuration lives in
//! the provider crates; server/TLS configuration lives in the binary.

/// Configuration common to every provider.
#[derive(Clone, Debug)]
pub struct CoreConfig {
    /// Root directory for cwii-managed mounts, e.g. `/var/run/secrets/cwii.dev`.
    pub mount_root: String,
    /// Default projected-token lifetime in seconds when a provider has no per-pod override.
    pub token_expiration_secs: i64,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            mount_root: "/var/run/secrets/cwii.dev".to_string(),
            token_expiration_secs: 3600,
        }
    }
}
