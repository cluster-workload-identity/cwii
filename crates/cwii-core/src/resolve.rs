//! Annotation resolution with the cwii precedence model.
//!
//! Annotations are evaluated, **independently per key**, in the order
//! pod > owning workload > ServiceAccount > namespace. The first map that contains an explicit
//! value wins, so a more specific `false` correctly suppresses a broader `true`, and one provider's
//! settings never affect another's.

use std::collections::BTreeMap;

use crate::annotations::inject_key;
use crate::provider::ProviderId;

/// Resolve a provider's enable decision: an explicit `cwii.dev/<p>-inject` always wins; otherwise
/// fall back to `native_present` (true only when native-annotation compatibility is enabled and the
/// provider's native annotation is set).
pub fn enabled_with_native(a: &AnnotationSet<'_>, id: ProviderId, native_present: bool) -> bool {
    a.first_explicit_bool(&inject_key(id))
        .unwrap_or(native_present)
}

/// First non-empty value of `cwii_key`, falling back to `native_key` only when `native_on`. Used to
/// let a managed-platform "native" annotation supply a value when no `cwii.dev/*` one is set.
pub fn native_or<'a>(
    a: &AnnotationSet<'a>,
    cwii_key: &str,
    native_key: &str,
    native_on: bool,
) -> Option<&'a str> {
    a.first_non_empty(cwii_key).or_else(|| {
        if native_on {
            a.first_non_empty(native_key)
        } else {
            None
        }
    })
}

/// A view over the four annotation sources, in precedence order.
#[derive(Debug, Default, Clone)]
pub struct AnnotationSet<'a> {
    pub pod: Option<&'a BTreeMap<String, String>>,
    pub owner: Option<&'a BTreeMap<String, String>>,
    pub service_account: Option<&'a BTreeMap<String, String>>,
    pub namespace: Option<&'a BTreeMap<String, String>>,
}

impl<'a> AnnotationSet<'a> {
    fn ordered(&self) -> [Option<&'a BTreeMap<String, String>>; 4] {
        [self.pod, self.owner, self.service_account, self.namespace]
    }

    /// First explicit boolean for `key` (`"true"`/`"false"`, case-insensitive), in precedence
    /// order. Non-boolean values are skipped rather than treated as an explicit answer.
    pub fn first_explicit_bool(&self, key: &str) -> Option<bool> {
        for m in self.ordered() {
            if let Some(raw) = m.and_then(|a| a.get(key)) {
                match raw.trim().to_ascii_lowercase().as_str() {
                    "true" => return Some(true),
                    "false" => return Some(false),
                    _ => {}
                }
            }
        }
        None
    }

    /// First non-empty (trimmed) string value for `key`, in precedence order.
    pub fn first_non_empty(&self, key: &str) -> Option<&'a str> {
        for m in self.ordered() {
            if let Some(raw) = m.and_then(|a| a.get(key)) {
                let t = raw.trim();
                if !t.is_empty() {
                    return Some(t);
                }
            }
        }
        None
    }

    /// First value for `key` that parses as an `i64`, in precedence order.
    pub fn first_i64(&self, key: &str) -> Option<i64> {
        self.first_non_empty(key).and_then(|s| s.parse().ok())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::AnnotationSet;

    fn m(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn pod_false_beats_namespace_true() {
        let pod = m(&[("cwii.dev/gcp-inject", "false")]);
        let ns = m(&[("cwii.dev/gcp-inject", "true")]);
        let a = AnnotationSet {
            pod: Some(&pod),
            namespace: Some(&ns),
            ..Default::default()
        };
        assert_eq!(a.first_explicit_bool("cwii.dev/gcp-inject"), Some(false));
    }

    #[test]
    fn owner_false_beats_namespace_true() {
        let owner = m(&[("cwii.dev/gcp-inject", "false")]);
        let ns = m(&[("cwii.dev/gcp-inject", "true")]);
        let a = AnnotationSet {
            owner: Some(&owner),
            namespace: Some(&ns),
            ..Default::default()
        };
        assert_eq!(a.first_explicit_bool("cwii.dev/gcp-inject"), Some(false));
    }

    #[test]
    fn providers_resolve_independently() {
        // gcp disabled on the pod, aws enabled on the namespace — they must not interfere.
        let pod = m(&[("cwii.dev/gcp-inject", "false")]);
        let ns = m(&[("cwii.dev/aws-inject", "true")]);
        let a = AnnotationSet {
            pod: Some(&pod),
            namespace: Some(&ns),
            ..Default::default()
        };
        assert_eq!(a.first_explicit_bool("cwii.dev/gcp-inject"), Some(false));
        assert_eq!(a.first_explicit_bool("cwii.dev/aws-inject"), Some(true));
    }

    #[test]
    fn value_precedence_and_fallback() {
        let pod = m(&[("cwii.dev/gcp-audience", "pod-aud")]);
        let ns = m(&[("cwii.dev/gcp-audience", "ns-aud")]);
        let a = AnnotationSet {
            pod: Some(&pod),
            namespace: Some(&ns),
            ..Default::default()
        };
        assert_eq!(a.first_non_empty("cwii.dev/gcp-audience"), Some("pod-aud"));

        let empty = AnnotationSet::default();
        assert_eq!(empty.first_non_empty("cwii.dev/gcp-audience"), None);
    }

    #[test]
    fn parses_i64_and_ignores_garbage() {
        let pod = m(&[("cwii.dev/gcp-token-expiration", "1800")]);
        let a = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        assert_eq!(a.first_i64("cwii.dev/gcp-token-expiration"), Some(1800));

        let bad = m(&[("cwii.dev/gcp-token-expiration", "soon")]);
        let a = AnnotationSet {
            pod: Some(&bad),
            ..Default::default()
        };
        assert_eq!(a.first_i64("cwii.dev/gcp-token-expiration"), None);
    }
}
