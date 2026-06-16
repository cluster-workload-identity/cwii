//! GCP `external_account` credentials.json generation and deterministic ConfigMap naming.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Build the GCP Workload Identity Federation `external_account` credentials document.
///
/// `token_file` is the absolute path to the projected ServiceAccount token inside the pod (the
/// provider-specific token, not the default auto-mounted one). When `sa_email` is set, a
/// `service_account_impersonation_url` is added so the SDK impersonates that service account;
/// otherwise the workload federates directly.
pub fn build_credentials_json(audience: &str, token_file: &str, sa_email: Option<&str>) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("type".into(), json!("external_account"));
    obj.insert("audience".into(), json!(audience));
    obj.insert(
        "subject_token_type".into(),
        json!("urn:ietf:params:oauth:token-type:jwt"),
    );
    obj.insert(
        "token_url".into(),
        json!("https://sts.googleapis.com/v1/token"),
    );
    obj.insert(
        "token_info_url".into(),
        json!("https://sts.googleapis.com/v1/introspect"),
    );
    obj.insert("credential_source".into(), json!({ "file": token_file }));
    if let Some(sa) = sa_email {
        obj.insert(
            "service_account_impersonation_url".into(),
            json!(format!(
                "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/{sa}:generateAccessToken"
            )),
        );
    }
    serde_json::to_string(&Value::Object(obj)).expect("credentials.json serializable")
}

/// Deterministic ConfigMap name for a given (audience, service account) pair, so identical
/// configurations share one ConfigMap across pods.
pub fn configmap_name(audience: &str, sa: Option<&str>) -> String {
    let mut h = Sha256::new();
    h.update(audience.as_bytes());
    h.update([0]);
    if let Some(sa) = sa {
        h.update(sa.as_bytes());
    }
    let digest = h.finalize();
    let hex = hex::encode(&digest[..6]);
    format!("cwii-gcp-creds-{hex}")
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn credentials_json_minimal() {
        let s = build_credentials_json(
            "//iam.googleapis.com/projects/1/locations/global/workloadIdentityPools/p/providers/pr",
            "/var/run/secrets/cwii.dev/gcp/token",
            None,
        );
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["type"], "external_account");
        assert_eq!(
            v["credential_source"]["file"],
            "/var/run/secrets/cwii.dev/gcp/token"
        );
        assert!(v.get("service_account_impersonation_url").is_none());
    }

    #[test]
    fn credentials_json_with_impersonation() {
        let s = build_credentials_json("aud", "/tok", Some("svc@proj.iam.gserviceaccount.com"));
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(
            v["service_account_impersonation_url"],
            "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/svc@proj.iam.gserviceaccount.com:generateAccessToken",
        );
    }

    #[test]
    fn configmap_name_is_stable_and_distinct() {
        let a = configmap_name("aud1", None);
        assert_eq!(a, configmap_name("aud1", None));
        assert_ne!(a, configmap_name("aud2", None));
        assert_ne!(
            a,
            configmap_name("aud1", Some("svc@x.iam.gserviceaccount.com"))
        );
        assert!(a.starts_with("cwii-gcp-creds-"));
    }
}
