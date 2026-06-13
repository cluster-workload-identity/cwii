//! GCP Workload Identity Federation provider for cwii.
//!
//! Injects an `external_account` credentials.json (delivered via a ConfigMap or an init container)
//! plus a per-provider projected ServiceAccount token, and sets `GOOGLE_APPLICATION_CREDENTIALS`.

pub mod credentials;

use std::str::FromStr;

use cwii_core::annotations::{audience_key, verify_enforce_key, verify_image_key, verify_key};
use cwii_core::{
    AnnotationSet, ConfigMapUpsert, EnvVar, InitContainer, Provider, ProviderContext, ProviderId,
    ProviderPlan, VolumeMount, VolumeSpec, enabled_with_native, hardened_writer_security_context,
    native_or, projected_token, verify_init_container, writer_resources,
};
use serde_json::json;

const K_SERVICE_ACCOUNT: &str = "cwii.dev/gcp-service-account";
const K_DELIVERY: &str = "cwii.dev/gcp-delivery";
/// GKE's native Workload Identity annotation (the GSA to impersonate).
const NATIVE_SERVICE_ACCOUNT: &str = "iam.gke.io/gcp-service-account";
const CREDS_VOLUME: &str = "cwii-gcp-creds";
const CREDS_FILENAME: &str = "credentials.json";
/// Directory the init-container writer mounts the (writable) emptyDir at.
const WRITER_DIR: &str = "/cwii/gcp";

/// How the GCP credentials.json reaches the pod.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum GcpDelivery {
    /// Upsert a per-config ConfigMap and mount it (needs ConfigMap write RBAC).
    #[default]
    ConfigMap,
    /// Write credentials.json from an init container into an emptyDir (no cluster writes).
    InitContainer,
}

impl FromStr for GcpDelivery {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "config-map" | "configmap" => Ok(Self::ConfigMap),
            "init-container" | "initcontainer" => Ok(Self::InitContainer),
            other => Err(format!("unknown gcp delivery mode: {other}")),
        }
    }
}

/// Server-level GCP configuration (per-pod annotations may override some of these).
#[derive(Clone, Debug)]
pub struct GcpConfig {
    pub default_audience: Option<String>,
    pub delivery: GcpDelivery,
    pub init_image: String,
    pub verify_image: String,
    /// Also read GKE's `iam.gke.io/gcp-service-account` as a fallback.
    pub native_annotations: bool,
}

impl Default for GcpConfig {
    fn default() -> Self {
        Self {
            default_audience: None,
            delivery: GcpDelivery::ConfigMap,
            init_image: "busybox:stable".to_string(),
            verify_image: "google/cloud-sdk:slim".to_string(),
            native_annotations: false,
        }
    }
}

pub struct GcpProvider {
    cfg: GcpConfig,
}

impl GcpProvider {
    pub fn new(cfg: GcpConfig) -> Self {
        Self { cfg }
    }
}

impl Provider for GcpProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Gcp
    }

    fn enabled(&self, a: &AnnotationSet<'_>) -> bool {
        let native =
            self.cfg.native_annotations && a.first_non_empty(NATIVE_SERVICE_ACCOUNT).is_some();
        enabled_with_native(a, ProviderId::Gcp, native)
    }

    fn plan(&self, ctx: &ProviderContext<'_>) -> anyhow::Result<Option<ProviderPlan>> {
        let id = ProviderId::Gcp;
        let root = ctx.mount_root.trim_end_matches('/');

        let audience = ctx
            .annotations
            .first_non_empty(&audience_key(id))
            .map(str::to_owned)
            .or_else(|| self.cfg.default_audience.clone())
            .unwrap_or_default();
        if audience.is_empty() {
            tracing::warn!(
                namespace = ctx.namespace,
                "gcp: enabled but no audience (set cwii.dev/gcp-audience or a default)",
            );
            return Ok(None);
        }

        let token = projected_token(ctx, id, audience.clone());
        let token_file = token.token_file_path();

        let sa_email = native_or(
            ctx.annotations,
            K_SERVICE_ACCOUNT,
            NATIVE_SERVICE_ACCOUNT,
            self.cfg.native_annotations,
        )
        .map(str::to_owned);
        let creds_json =
            credentials::build_credentials_json(&audience, &token_file, sa_email.as_deref());

        let creds_dir = format!("{root}/gcp-creds");
        let creds_file = format!("{creds_dir}/{CREDS_FILENAME}");

        let delivery = ctx
            .annotations
            .first_non_empty(K_DELIVERY)
            .and_then(|s| GcpDelivery::from_str(s).ok())
            .unwrap_or(self.cfg.delivery);

        let mut volumes = vec![token.volume()];
        let mut container_mounts = vec![token.mount()];
        let mut init_containers = Vec::new();
        let mut configmap_upsert = None;

        match delivery {
            GcpDelivery::ConfigMap => {
                let cm_name = credentials::configmap_name(&audience, sa_email.as_deref());
                volumes.push(VolumeSpec {
                    name: CREDS_VOLUME.to_string(),
                    value: json!({
                        "name": CREDS_VOLUME,
                        "configMap": {
                            "name": cm_name,
                            "items": [{ "key": CREDS_FILENAME, "path": CREDS_FILENAME }]
                        }
                    }),
                });
                configmap_upsert = Some(ConfigMapUpsert {
                    name: cm_name,
                    data_key: CREDS_FILENAME.to_string(),
                    data_value: creds_json.clone(),
                });
            }
            GcpDelivery::InitContainer => {
                volumes.push(VolumeSpec {
                    name: CREDS_VOLUME.to_string(),
                    value: json!({ "name": CREDS_VOLUME, "emptyDir": {} }),
                });
                init_containers.push(InitContainer {
                    name: "cwii-gcp-creds-writer".to_string(),
                    image: self.cfg.init_image.clone(),
                    command: vec!["sh".to_string(), "-c".to_string()],
                    args: vec![format!(
                        "printf '%s' \"$CWII_GCP_CREDS_JSON\" > {WRITER_DIR}/{CREDS_FILENAME}"
                    )],
                    env: vec![EnvVar {
                        name: "CWII_GCP_CREDS_JSON".to_string(),
                        value: creds_json.clone(),
                    }],
                    mounts: vec![VolumeMount {
                        name: CREDS_VOLUME.to_string(),
                        mount_path: WRITER_DIR.to_string(),
                        read_only: false,
                    }],
                    security_context: hardened_writer_security_context(),
                    resources: writer_resources(),
                    order: 0,
                });
            }
        }

        container_mounts.push(VolumeMount {
            name: CREDS_VOLUME.to_string(),
            mount_path: creds_dir.clone(),
            read_only: true,
        });

        let container_env = vec![EnvVar {
            name: "GOOGLE_APPLICATION_CREDENTIALS".to_string(),
            value: creds_file.clone(),
        }];

        if ctx
            .annotations
            .first_explicit_bool(&verify_key(id))
            .unwrap_or(false)
        {
            let enforce = ctx
                .annotations
                .first_explicit_bool(&verify_enforce_key(id))
                .unwrap_or(false);
            let image = ctx
                .annotations
                .first_non_empty(&verify_image_key(id))
                .map(str::to_owned)
                .unwrap_or_else(|| self.cfg.verify_image.clone());
            // ADC resolution honours GOOGLE_APPLICATION_CREDENTIALS, so this exercises the full
            // STS token exchange (and impersonation hop, if configured).
            init_containers.push(verify_init_container(
                id,
                image,
                "gcloud auth application-default print-access-token >/dev/null",
                enforce,
                container_env.clone(),
                vec![
                    token.mount(),
                    VolumeMount {
                        name: CREDS_VOLUME.to_string(),
                        mount_path: creds_dir.clone(),
                        read_only: true,
                    },
                ],
            ));
        }

        Ok(Some(ProviderPlan {
            provider: id,
            volumes,
            container_mounts,
            container_env,
            init_containers,
            configmap_upsert,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use cwii_core::AnnotationSet;

    use super::*;

    fn ctx_with<'a>(annos: &'a AnnotationSet<'a>) -> ProviderContext<'a> {
        ProviderContext {
            annotations: annos,
            namespace: "demo",
            service_account_name: "default",
            mount_root: "/var/run/secrets/cwii.dev",
            default_token_expiration_secs: 3600,
        }
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn configmap_delivery_emits_configmap_and_no_writer() {
        let pod = map(&[("cwii.dev/gcp-audience", "//iam.googleapis.com/x")]);
        let annos = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        let p = GcpProvider::new(GcpConfig::default())
            .plan(&ctx_with(&annos))
            .unwrap()
            .unwrap();
        assert!(p.configmap_upsert.is_some());
        assert!(p.init_containers.is_empty());
        assert!(p.volumes.iter().any(|v| v.name == "cwii-gcp-token"));
        assert!(
            p.container_env
                .iter()
                .any(|e| e.name == "GOOGLE_APPLICATION_CREDENTIALS")
        );
    }

    #[test]
    fn initcontainer_delivery_emits_writer_no_configmap() {
        let pod = map(&[
            ("cwii.dev/gcp-audience", "//iam.googleapis.com/x"),
            ("cwii.dev/gcp-delivery", "init-container"),
        ]);
        let annos = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        let p = GcpProvider::new(GcpConfig::default())
            .plan(&ctx_with(&annos))
            .unwrap()
            .unwrap();
        assert!(p.configmap_upsert.is_none());
        assert_eq!(p.init_containers[0].name, "cwii-gcp-creds-writer");
    }

    #[test]
    fn missing_audience_skips() {
        let annos = AnnotationSet::default();
        let p = GcpProvider::new(GcpConfig::default())
            .plan(&ctx_with(&annos))
            .unwrap();
        assert!(p.is_none());
    }

    #[test]
    fn verify_adds_ordered_init_container() {
        let pod = map(&[
            ("cwii.dev/gcp-audience", "//iam.googleapis.com/x"),
            ("cwii.dev/gcp-delivery", "init-container"),
            ("cwii.dev/gcp-verify", "true"),
        ]);
        let annos = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        let p = GcpProvider::new(GcpConfig::default())
            .plan(&ctx_with(&annos))
            .unwrap()
            .unwrap();
        let verify = p
            .init_containers
            .iter()
            .find(|c| c.name == "cwii-gcp-verify")
            .unwrap();
        assert_eq!(verify.order, 10);
    }

    #[test]
    fn native_annotation_enables_and_supplies_gsa_when_compat_on() {
        let cfg = GcpConfig {
            native_annotations: true,
            default_audience: Some("//iam.googleapis.com/x".to_string()),
            ..Default::default()
        };
        let pod = map(&[(
            "iam.gke.io/gcp-service-account",
            "gke@proj.iam.gserviceaccount.com",
        )]);
        let annos = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        let provider = GcpProvider::new(cfg);
        assert!(provider.enabled(&annos));
        let p = provider.plan(&ctx_with(&annos)).unwrap().unwrap();
        let cm = p.configmap_upsert.unwrap();
        assert!(cm.data_value.contains("gke@proj.iam.gserviceaccount.com"));
    }

    #[test]
    fn cwii_annotation_wins_over_native() {
        let cfg = GcpConfig {
            native_annotations: true,
            default_audience: Some("//iam.googleapis.com/x".to_string()),
            ..Default::default()
        };
        let pod = map(&[
            (
                "iam.gke.io/gcp-service-account",
                "gke@proj.iam.gserviceaccount.com",
            ),
            (
                "cwii.dev/gcp-service-account",
                "cwii@proj.iam.gserviceaccount.com",
            ),
        ]);
        let annos = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        let p = GcpProvider::new(cfg)
            .plan(&ctx_with(&annos))
            .unwrap()
            .unwrap();
        let cm = p.configmap_upsert.unwrap();
        assert!(cm.data_value.contains("cwii@proj.iam.gserviceaccount.com"));
        assert!(!cm.data_value.contains("gke@proj"));
    }

    #[test]
    fn native_annotation_ignored_when_compat_off() {
        let pod = map(&[(
            "iam.gke.io/gcp-service-account",
            "gke@proj.iam.gserviceaccount.com",
        )]);
        let annos = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        // Default config has native_annotations = false.
        assert!(!GcpProvider::new(GcpConfig::default()).enabled(&annos));
    }
}
