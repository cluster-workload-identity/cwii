//! AWS provider for cwii (self-hosted clusters, IRSA-style `AssumeRoleWithWebIdentity`).
//!
//! Injects a per-provider projected ServiceAccount token (audience `sts.amazonaws.com` by default)
//! and the env vars the AWS SDKs read — `AWS_ROLE_ARN` and `AWS_WEB_IDENTITY_TOKEN_FILE`, plus
//! optional region/session — and adds an optional `aws sts get-caller-identity` verifier. There is
//! no credentials file, so the ConfigMap/init-container delivery modes do not apply here.

use cwii_core::annotations::{audience_key, verify_enforce_key, verify_image_key, verify_key};
use cwii_core::{
    AnnotationSet, EnvVar, Provider, ProviderContext, ProviderId, ProviderPlan,
    enabled_with_native, native_or, projected_token, verify_init_container,
};

const K_ROLE_ARN: &str = "cwii.dev/aws-role-arn";
const K_REGION: &str = "cwii.dev/aws-region";
const K_SESSION_NAME: &str = "cwii.dev/aws-role-session-name";
/// EKS's native IRSA annotation (the role ARN to assume).
const NATIVE_ROLE_ARN: &str = "eks.amazonaws.com/role-arn";

/// Server-level AWS configuration.
#[derive(Clone, Debug)]
pub struct AwsConfig {
    pub default_audience: String,
    pub verify_image: String,
    /// Also read EKS's `eks.amazonaws.com/role-arn` as a fallback.
    pub native_annotations: bool,
}

impl Default for AwsConfig {
    fn default() -> Self {
        Self {
            default_audience: "sts.amazonaws.com".to_string(),
            verify_image: "amazon/aws-cli:2.35.4".to_string(),
            native_annotations: false,
        }
    }
}

pub struct AwsProvider {
    cfg: AwsConfig,
}

impl AwsProvider {
    pub fn new(cfg: AwsConfig) -> Self {
        Self { cfg }
    }
}

impl Provider for AwsProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Aws
    }

    fn enabled(&self, a: &AnnotationSet<'_>) -> bool {
        let native = self.cfg.native_annotations && a.first_non_empty(NATIVE_ROLE_ARN).is_some();
        enabled_with_native(a, ProviderId::Aws, native)
    }

    fn plan(&self, ctx: &ProviderContext<'_>) -> anyhow::Result<Option<ProviderPlan>> {
        let id = ProviderId::Aws;

        let Some(role_arn) = native_or(
            ctx.annotations,
            K_ROLE_ARN,
            NATIVE_ROLE_ARN,
            self.cfg.native_annotations,
        )
        .map(str::to_owned) else {
            tracing::warn!(
                namespace = ctx.namespace,
                "aws: enabled but no role ARN (set cwii.dev/aws-role-arn)",
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
                name: "AWS_ROLE_ARN".to_string(),
                value: role_arn,
            },
            EnvVar {
                name: "AWS_WEB_IDENTITY_TOKEN_FILE".to_string(),
                value: token_file,
            },
        ];
        if let Some(region) = ctx.annotations.first_non_empty(K_REGION) {
            container_env.push(EnvVar {
                name: "AWS_REGION".to_string(),
                value: region.to_string(),
            });
        }
        if let Some(session) = ctx.annotations.first_non_empty(K_SESSION_NAME) {
            container_env.push(EnvVar {
                name: "AWS_ROLE_SESSION_NAME".to_string(),
                value: session.to_string(),
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
            init_containers.push(verify_init_container(
                id,
                image,
                "aws sts get-caller-identity",
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
    fn emits_env_only_no_file() {
        let pod = map(&[("cwii.dev/aws-role-arn", "arn:aws:iam::123:role/app")]);
        let annos = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        let p = AwsProvider::new(AwsConfig::default())
            .plan(&ctx_with(&annos))
            .unwrap()
            .unwrap();
        assert!(p.configmap_upsert.is_none());
        assert!(p.init_containers.is_empty());
        assert!(p.container_env.iter().any(|e| e.name == "AWS_ROLE_ARN"));
        assert!(
            p.container_env
                .iter()
                .any(|e| e.name == "AWS_WEB_IDENTITY_TOKEN_FILE"
                    && e.value == "/var/run/secrets/cwii.dev/aws/token")
        );
        // Default audience on the projected token.
        let v = &p.volumes[0].value;
        assert_eq!(
            v["projected"]["sources"][0]["serviceAccountToken"]["audience"],
            "sts.amazonaws.com"
        );
    }

    #[test]
    fn skips_without_role_arn() {
        let annos = AnnotationSet::default();
        let p = AwsProvider::new(AwsConfig::default())
            .plan(&ctx_with(&annos))
            .unwrap();
        assert!(p.is_none());
    }

    #[test]
    fn optional_region_and_session() {
        let pod = map(&[
            ("cwii.dev/aws-role-arn", "arn:aws:iam::123:role/app"),
            ("cwii.dev/aws-region", "eu-west-1"),
            ("cwii.dev/aws-role-session-name", "demo-session"),
        ]);
        let annos = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        let p = AwsProvider::new(AwsConfig::default())
            .plan(&ctx_with(&annos))
            .unwrap()
            .unwrap();
        assert!(
            p.container_env
                .iter()
                .any(|e| e.name == "AWS_REGION" && e.value == "eu-west-1")
        );
        assert!(
            p.container_env
                .iter()
                .any(|e| e.name == "AWS_ROLE_SESSION_NAME")
        );
    }

    #[test]
    fn native_eks_annotation_when_compat_on() {
        let cfg = AwsConfig {
            native_annotations: true,
            ..Default::default()
        };
        let pod = map(&[("eks.amazonaws.com/role-arn", "arn:aws:iam::123:role/eks")]);
        let annos = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        let provider = AwsProvider::new(cfg);
        assert!(provider.enabled(&annos));
        let p = provider.plan(&ctx_with(&annos)).unwrap().unwrap();
        assert!(
            p.container_env
                .iter()
                .any(|e| e.name == "AWS_ROLE_ARN" && e.value == "arn:aws:iam::123:role/eks")
        );
    }

    #[test]
    fn native_ignored_when_compat_off() {
        let pod = map(&[("eks.amazonaws.com/role-arn", "arn:aws:iam::123:role/eks")]);
        let annos = AnnotationSet {
            pod: Some(&pod),
            ..Default::default()
        };
        assert!(!AwsProvider::new(AwsConfig::default()).enabled(&annos));
    }
}
