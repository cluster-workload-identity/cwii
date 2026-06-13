//! The intermediate representation providers emit, and the merge that combines them.
//!
//! Providers describe *what* to inject (volumes, mounts, env, init containers, an optional
//! ConfigMap to upsert). [`crate::patch`] decides *how* — it alone knows JSON-pointer syntax and
//! the pod's current shape.

use serde_json::{Value, json};

use crate::annotations::token_expiration_key;
use crate::provider::{ProviderContext, ProviderId};

/// Kubernetes-enforced minimum lifetime for a projected ServiceAccount token.
pub const MIN_TOKEN_EXPIRATION_SECS: i64 = 600;

/// Clamp a requested token expiration up to the Kubernetes minimum.
pub fn clamp_token_expiration(secs: i64) -> i64 {
    secs.max(MIN_TOKEN_EXPIRATION_SECS)
}

/// Build a provider's projected-token spec, applying the shared conventions: mount directory
/// `<mount_root>/<abbr>`, file name `token`, and a per-pod-overridable, clamped expiration. Each
/// provider supplies only its resolved `audience`.
pub fn projected_token(ctx: &ProviderContext<'_>, id: ProviderId, audience: String) -> TokenSpec {
    let expiration_secs = clamp_token_expiration(
        ctx.annotations
            .first_i64(&token_expiration_key(id))
            .unwrap_or(ctx.default_token_expiration_secs),
    );
    TokenSpec {
        provider: id,
        audience,
        expiration_secs,
        mount_dir: format!("{}/{}", ctx.mount_root.trim_end_matches('/'), id.abbr()),
        file_name: "token".to_string(),
    }
}

/// A complete Kubernetes `Volume` object plus its name (for de-duplication).
#[derive(Debug, Clone)]
pub struct VolumeSpec {
    pub name: String,
    /// The full volume object, e.g. `{"name":"cwii-gcp-token","projected":{…}}`.
    pub value: Value,
}

/// A container `volumeMount`.
#[derive(Debug, Clone)]
pub struct VolumeMount {
    pub name: String,
    pub mount_path: String,
    pub read_only: bool,
}

impl VolumeMount {
    pub fn to_value(&self) -> Value {
        json!({
            "name": self.name,
            "mountPath": self.mount_path,
            "readOnly": self.read_only,
        })
    }
}

/// A container environment variable (literal value form).
#[derive(Debug, Clone)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

impl EnvVar {
    pub fn to_value(&self) -> Value {
        json!({ "name": self.name, "value": self.value })
    }
}

/// An init container cwii injects (a GCP credentials writer, or a verification "can-i" container).
#[derive(Debug, Clone)]
pub struct InitContainer {
    pub name: String,
    pub image: String,
    pub command: Vec<String>,
    pub args: Vec<String>,
    pub env: Vec<EnvVar>,
    pub mounts: Vec<VolumeMount>,
    pub security_context: Value,
    pub resources: Value,
    /// Relative ordering among cwii's own init containers: lower runs first. Writers use `0`,
    /// verifiers `10`, so a verifier always runs after the credentials it checks are in place.
    pub order: i32,
}

impl InitContainer {
    pub fn to_value(&self) -> Value {
        json!({
            "name": self.name,
            "image": self.image,
            "imagePullPolicy": "IfNotPresent",
            "command": self.command,
            "args": self.args,
            "env": self.env.iter().map(EnvVar::to_value).collect::<Vec<_>>(),
            "volumeMounts": self.mounts.iter().map(VolumeMount::to_value).collect::<Vec<_>>(),
            "securityContext": self.security_context,
            "resources": self.resources,
        })
    }
}

/// A projected ServiceAccount-token volume for one provider. Renders to a `projected` volume with a
/// single `serviceAccountToken` source carrying the provider-specific audience.
#[derive(Debug, Clone)]
pub struct TokenSpec {
    pub provider: ProviderId,
    pub audience: String,
    pub expiration_secs: i64,
    /// Directory the volume is mounted at, e.g. `/var/run/secrets/cwii.dev/gcp`.
    pub mount_dir: String,
    /// File name within the volume, e.g. `token`.
    pub file_name: String,
}

impl TokenSpec {
    pub fn volume_name(&self) -> String {
        format!("cwii-{}-token", self.provider.abbr())
    }

    /// Absolute path to the token file inside the container.
    pub fn token_file_path(&self) -> String {
        format!(
            "{}/{}",
            self.mount_dir.trim_end_matches('/'),
            self.file_name
        )
    }

    pub fn volume(&self) -> VolumeSpec {
        let name = self.volume_name();
        VolumeSpec {
            value: json!({
                "name": name,
                "projected": {
                    "sources": [{
                        "serviceAccountToken": {
                            "audience": self.audience,
                            "expirationSeconds": self.expiration_secs,
                            "path": self.file_name,
                        }
                    }]
                }
            }),
            name,
        }
    }

    pub fn mount(&self) -> VolumeMount {
        VolumeMount {
            name: self.volume_name(),
            mount_path: self.mount_dir.clone(),
            read_only: true,
        }
    }
}

/// A ConfigMap the webhook must server-side-apply before the patch is applied (GCP ConfigMap
/// delivery only).
#[derive(Debug, Clone)]
pub struct ConfigMapUpsert {
    pub name: String,
    pub data_key: String,
    pub data_value: String,
}

/// One provider's contribution to a pod mutation.
#[derive(Debug, Clone)]
pub struct ProviderPlan {
    pub provider: ProviderId,
    /// Volumes to add (the projected token volume, plus any GCP creds volume).
    pub volumes: Vec<VolumeSpec>,
    /// Mounts applied to every main container.
    pub container_mounts: Vec<VolumeMount>,
    /// Env vars applied to every main container.
    pub container_env: Vec<EnvVar>,
    /// Init containers (writer and/or verifier).
    pub init_containers: Vec<InitContainer>,
    /// Optional ConfigMap to upsert.
    pub configmap_upsert: Option<ConfigMapUpsert>,
}

/// The merged plan for a whole pod, ready to render into patches.
#[derive(Debug, Default)]
pub struct MutationPlan {
    pub volumes: Vec<VolumeSpec>,
    pub container_mounts: Vec<VolumeMount>,
    pub container_env: Vec<EnvVar>,
    pub init_containers: Vec<InitContainer>,
    pub configmap_upserts: Vec<ConfigMapUpsert>,
    pub injected_providers: Vec<ProviderId>,
}

/// Merge provider plans in registry order. Init containers are stably sorted by `order` so
/// credentials writers (`0`) precede verifiers (`10`) regardless of provider iteration order.
pub fn merge(plans: Vec<ProviderPlan>) -> MutationPlan {
    let mut out = MutationPlan::default();
    for p in plans {
        out.injected_providers.push(p.provider);
        out.volumes.extend(p.volumes);
        out.container_mounts.extend(p.container_mounts);
        out.container_env.extend(p.container_env);
        out.init_containers.extend(p.init_containers);
        out.configmap_upserts.extend(p.configmap_upsert);
    }
    out.init_containers.sort_by_key(|c| c.order);
    out
}

/// Build a provider's "can-i" verification init container, hardcoding the shared invariants (name
/// `cwii-<abbr>-verify`, security context, resources, ordering after the writer). Non-enforcing
/// checks are wrapped so the container always exits 0 (the failure is only logged); enforcing
/// checks run bare, so a non-zero exit blocks pod startup.
pub fn verify_init_container(
    id: ProviderId,
    image: String,
    check: &str,
    enforce: bool,
    env: Vec<EnvVar>,
    mounts: Vec<VolumeMount>,
) -> InitContainer {
    let args = if enforce {
        vec![check.to_string()]
    } else {
        vec![format!(
            "{check} || echo 'cwii: verification check failed (non-enforcing)' >&2"
        )]
    };
    InitContainer {
        name: format!("cwii-{}-verify", id.abbr()),
        image,
        command: vec!["sh".to_string(), "-c".to_string()],
        args,
        env,
        mounts,
        security_context: verify_security_context(),
        resources: writer_resources(),
        order: 10,
    }
}

/// Hardened security context for a credentials-writer init container (writes only to an emptyDir,
/// so the root filesystem can stay read-only).
pub fn hardened_writer_security_context() -> Value {
    json!({
        "allowPrivilegeEscalation": false,
        "readOnlyRootFilesystem": true,
        "runAsNonRoot": false,
        "capabilities": { "drop": ["ALL"] }
    })
}

/// Security context for a verification init container. Cloud CLIs may write to a config/home dir,
/// so the root filesystem is left writable; capabilities are still dropped and privilege escalation
/// forbidden.
pub fn verify_security_context() -> Value {
    json!({
        "allowPrivilegeEscalation": false,
        "capabilities": { "drop": ["ALL"] }
    })
}

/// Small resource requests/limits shared by cwii's init containers.
pub fn writer_resources() -> Value {
    json!({
        "requests": { "cpu": "10m", "memory": "16Mi" },
        "limits": { "cpu": "100m", "memory": "32Mi" }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_volume_carries_audience_and_path() {
        let t = TokenSpec {
            provider: ProviderId::Gcp,
            audience: "//iam.googleapis.com/x".into(),
            expiration_secs: 3600,
            mount_dir: "/var/run/secrets/cwii.dev/gcp".into(),
            file_name: "token".into(),
        };
        assert_eq!(t.volume_name(), "cwii-gcp-token");
        assert_eq!(t.token_file_path(), "/var/run/secrets/cwii.dev/gcp/token");
        let v = t.volume().value;
        let src = &v["projected"]["sources"][0]["serviceAccountToken"];
        assert_eq!(src["audience"], "//iam.googleapis.com/x");
        assert_eq!(src["expirationSeconds"], 3600);
        assert_eq!(src["path"], "token");
    }

    #[test]
    fn merge_orders_writers_before_verifiers() {
        let mk = |name: &str, order: i32| InitContainer {
            name: name.into(),
            image: "img".into(),
            command: vec![],
            args: vec![],
            env: vec![],
            mounts: vec![],
            security_context: json!({}),
            resources: json!({}),
            order,
        };
        let plan = ProviderPlan {
            provider: ProviderId::Gcp,
            volumes: vec![],
            container_mounts: vec![],
            container_env: vec![],
            init_containers: vec![mk("verify", 10), mk("writer", 0)],
            configmap_upsert: None,
        };
        let merged = merge(vec![plan]);
        assert_eq!(merged.init_containers[0].name, "writer");
        assert_eq!(merged.init_containers[1].name, "verify");
    }

    #[test]
    fn verify_init_container_enforce_vs_non_enforce() {
        let c = verify_init_container(
            ProviderId::Aws,
            "img".into(),
            "check",
            false,
            vec![],
            vec![],
        );
        assert_eq!(c.name, "cwii-aws-verify");
        assert_eq!(c.order, 10);
        assert!(c.args[0].contains("|| echo"));

        let c = verify_init_container(ProviderId::Aws, "img".into(), "check", true, vec![], vec![]);
        assert_eq!(c.args[0], "check");
    }

    #[test]
    fn clamp_respects_minimum() {
        assert_eq!(clamp_token_expiration(60), MIN_TOKEN_EXPIRATION_SECS);
        assert_eq!(clamp_token_expiration(3600), 3600);
    }
}
