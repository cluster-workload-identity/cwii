//! Azure (Microsoft Entra ID) workload identity federation provider for cwii.
//!
//! Like AWS, this is env-vars only: it injects a per-provider projected ServiceAccount token
//! (default audience `api://AzureADTokenExchange`) and the env vars the Azure SDKs / `DefaultAzure`
//! credential read — `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_FEDERATED_TOKEN_FILE`, and
//! optionally `AZURE_AUTHORITY_HOST`. The app registration / managed identity must have a federated
//! identity credential trusting the cluster issuer + ServiceAccount subject. There is no
//! credentials file, so the ConfigMap/init-container delivery modes do not apply here.

use cwii_core::annotations::{audience_key, verify_enforce_key, verify_image_key, verify_key};
use cwii_core::{
    AnnotationSet, EnvVar, Provider, ProviderContext, ProviderId, ProviderPlan,
    enabled_with_native, native_or, projected_token, verify_init_container,
};

const K_CLIENT_ID: &str = "cwii.dev/az-client-id";
const K_TENANT_ID: &str = "cwii.dev/az-tenant-id";
const K_AUTHORITY_HOST: &str = "cwii.dev/az-authority-host";
/// Azure Workload Identity's native annotations (on the ServiceAccount).
const NATIVE_CLIENT_ID: &str = "azure.workload.identity/client-id";
const NATIVE_TENANT_ID: &str = "azure.workload.identity/tenant-id";

/// Server-level Azure configuration.
#[derive(Clone, Debug)]
pub struct AzConfig {
    pub default_audience: String,
    pub verify_image: String,
    /// Also read `azure.workload.identity/{client-id,tenant-id}` as a fallback.
    pub native_annotations: bool,
}

impl Default for AzConfig {
    fn default() -> Self {
        Self {
            default_audience: "api://AzureADTokenExchange".to_string(),
            verify_image: "mcr.microsoft.com/azure-cli:latest".to_string(),
            native_annotations: false,
        }
    }
}

pub struct AzProvider {
    cfg: AzConfig,
}

impl AzProvider {
    pub fn new(cfg: AzConfig) -> Self {
        Self { cfg }
    }
}

impl Provider for AzProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Az
    }

    fn enabled(&self, a: &AnnotationSet<'_>) -> bool {
        let native = self.cfg.native_annotations && a.first_non_empty(NATIVE_CLIENT_ID).is_some();
        enabled_with_native(a, ProviderId::Az, native)
    }

    fn plan(&self, ctx: &ProviderContext<'_>) -> anyhow::Result<Option<ProviderPlan>> {
        let id = ProviderId::Az;

        let native = self.cfg.native_annotations;
        let client_id = native_or(ctx.annotations, K_CLIENT_ID, NATIVE_CLIENT_ID, native);
        let tenant_id = native_or(ctx.annotations, K_TENANT_ID, NATIVE_TENANT_ID, native);
        let (Some(client_id), Some(tenant_id)) = (client_id, tenant_id) else {
            tracing::warn!(
                namespace = ctx.namespace,
                "az: enabled but missing client/tenant id (set cwii.dev/az-client-id and \
                 cwii.dev/az-tenant-id)",
            );
            return Ok(None);
        };

        let audience = ctx
            .annotations
            .first_non_empty(&audience_key(id))
            .map(str::to_owned)
            .unwrap_or_else(|| self.cfg.default_audience.clone());

        let token = projected_token(ctx, id, audience);
        let token_file = token.token_file_path();

        let mut container_env = vec![
            EnvVar {
                name: "AZURE_CLIENT_ID".to_string(),
                value: client_id.to_string(),
            },
            EnvVar {
                name: "AZURE_TENANT_ID".to_string(),
                value: tenant_id.to_string(),
            },
            EnvVar {
                name: "AZURE_FEDERATED_TOKEN_FILE".to_string(),
                value: token_file,
            },
        ];
        if let Some(authority) = ctx.annotations.first_non_empty(K_AUTHORITY_HOST) {
            container_env.push(EnvVar {
                name: "AZURE_AUTHORITY_HOST".to_string(),
                value: authority.to_string(),
            });
        }

        let mut init_containers = Vec::new();
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
            let check = "az login --service-principal -u \"$AZURE_CLIENT_ID\" -t \
                         \"$AZURE_TENANT_ID\" --federated-token \"$(cat \
                         \"$AZURE_FEDERATED_TOKEN_FILE\")\" >/dev/null && az account show \
                         >/dev/null";
            init_containers.push(verify_init_container(
                id,
                image,
                check,
                enforce,
                container_env.clone(),
                vec![token.mount()],
            ));
        }

        Ok(Some(ProviderPlan {
            provider: id,
            volumes: vec![token.volume()],
            container_mounts: vec![token.mount()],
            container_env,
            init_containers,
            configmap_upsert: None,
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
    fn emits_env_with_default_audience() {
        let pod = map(&[
            (
                "cwii.dev/az-client-id",
                "11111111-1111-1111-1111-111111111111",
            ),
            (
                "cwii.dev/az-tenant-id",
                "22222222-2222-2222-2222-222222222222",
            ),
        ]);
        let annos = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        let p = AzProvider::new(AzConfig::default())
            .plan(&ctx_with(&annos))
            .unwrap()
            .unwrap();
        assert!(p.container_env.iter().any(|e| e.name == "AZURE_CLIENT_ID"));
        assert!(
            p.container_env
                .iter()
                .any(|e| e.name == "AZURE_FEDERATED_TOKEN_FILE"
                    && e.value == "/var/run/secrets/cwii.dev/az/token")
        );
        let v = &p.volumes[0].value;
        assert_eq!(
            v["projected"]["sources"][0]["serviceAccountToken"]["audience"],
            "api://AzureADTokenExchange"
        );
    }

    #[test]
    fn skips_without_client_or_tenant() {
        let pod = map(&[("cwii.dev/az-client-id", "only-client")]);
        let annos = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        assert!(
            AzProvider::new(AzConfig::default())
                .plan(&ctx_with(&annos))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn native_azure_workload_identity_annotations_when_compat_on() {
        let cfg = AzConfig {
            native_annotations: true,
            ..Default::default()
        };
        let pod = map(&[
            ("azure.workload.identity/client-id", "native-client"),
            ("azure.workload.identity/tenant-id", "native-tenant"),
        ]);
        let annos = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        let provider = AzProvider::new(cfg);
        assert!(provider.enabled(&annos));
        let p = provider.plan(&ctx_with(&annos)).unwrap().unwrap();
        assert!(
            p.container_env
                .iter()
                .any(|e| e.name == "AZURE_CLIENT_ID" && e.value == "native-client")
        );
        assert!(
            p.container_env
                .iter()
                .any(|e| e.name == "AZURE_TENANT_ID" && e.value == "native-tenant")
        );
    }

    #[test]
    fn native_ignored_when_compat_off() {
        let pod = map(&[
            ("azure.workload.identity/client-id", "native-client"),
            ("azure.workload.identity/tenant-id", "native-tenant"),
        ]);
        let annos = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        assert!(!AzProvider::new(AzConfig::default()).enabled(&annos));
    }
}
