//! The [`Provider`] trait that each cloud implements, plus the [`ProviderContext`] handed to it.

use crate::annotations;
use crate::plan::ProviderPlan;
use crate::resolve::AnnotationSet;

/// Stable short identifier for a cloud provider. The [`ProviderId::abbr`] string is used verbatim
/// in annotation keys (`cwii.dev/<abbr>-…`), token mount paths (`…/<abbr>/token`), volume names
/// (`cwii-<abbr>-…`) and env values, so it must never change for an existing provider.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProviderId {
    Gcp,
    Aws,
    Az,
}

impl ProviderId {
    /// The canonical abbreviation: `"gcp"`, `"aws"` or `"az"`.
    pub fn abbr(self) -> &'static str {
        match self {
            Self::Gcp => "gcp",
            Self::Aws => "aws",
            Self::Az => "az",
        }
    }
}

/// Everything a provider needs to decide and build its plan, without a live cluster handle:
/// providers stay pure and synchronous. Side effects (such as a GCP ConfigMap upsert) are returned
/// as data in the [`ProviderPlan`] and executed later by the core admission flow.
pub struct ProviderContext<'a> {
    /// Resolved annotations across pod / owner / ServiceAccount / namespace.
    pub annotations: &'a AnnotationSet<'a>,
    /// The pod's namespace.
    pub namespace: &'a str,
    /// The pod's ServiceAccount name (`"default"` when unset).
    pub service_account_name: &'a str,
    /// Root directory for cwii-managed mounts, e.g. `/var/run/secrets/cwii.dev`.
    pub mount_root: &'a str,
    /// Default projected-token expiration when a provider has no per-pod override.
    pub default_token_expiration_secs: i64,
}

/// A cloud provider that can inject workload-identity plumbing into a pod.
///
/// Implementors are registered with the webhook at startup and consulted, in a fixed order, for
/// every admitted pod.
pub trait Provider: Send + Sync {
    /// This provider's identifier.
    fn id(&self) -> ProviderId;

    /// Whether this provider is enabled for the pod, via the `cwii.dev/<abbr>-inject` annotation
    /// resolved with the standard precedence. The default implementation suits every provider.
    fn enabled(&self, annotations: &AnnotationSet<'_>) -> bool {
        annotations
            .first_explicit_bool(&annotations::inject_key(self.id()))
            .unwrap_or(false)
    }

    /// Build this provider's contribution.
    ///
    /// - `Ok(Some(plan))` — inject the plan.
    /// - `Ok(None)` — skip (enabled but mis-configured, e.g. AWS without a role ARN); the provider
    ///   is expected to log the reason.
    /// - `Err(_)` — a hard failure that should *deny* the admission.
    fn plan(&self, ctx: &ProviderContext<'_>) -> anyhow::Result<Option<ProviderPlan>>;
}
