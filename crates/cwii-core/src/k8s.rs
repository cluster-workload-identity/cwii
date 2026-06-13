//! Kubernetes reads (owner / ServiceAccount / namespace annotations) and the ConfigMap upsert used
//! by GCP ConfigMap delivery.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use k8s_openapi::api::apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet};
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Pod, ServiceAccount};
use kube::api::{Api, Patch, PatchParams};
use kube::core::ObjectMeta;

/// Field manager / `managed-by` value for resources cwii server-side-applies.
pub const MANAGER: &str = "cwii";

/// Walk a pod's ownerReferences up to a workload (Deployment / StatefulSet / DaemonSet / Job),
/// returning that workload's annotations. For the Deployment → ReplicaSet → Pod chain the
/// Deployment's annotations are preferred over the ReplicaSet's.
pub async fn owner_annotations(
    client: &kube::Client,
    namespace: &str,
    pod: &Pod,
) -> Result<Option<BTreeMap<String, String>>> {
    let Some(refs) = pod.metadata.owner_references.as_ref() else {
        return Ok(None);
    };
    for owner in refs {
        match owner.kind.as_str() {
            "ReplicaSet" => {
                let rs_api: Api<ReplicaSet> = Api::namespaced(client.clone(), namespace);
                if let Some(rs) = rs_api.get_opt(&owner.name).await? {
                    if let Some(rs_owners) = &rs.metadata.owner_references {
                        for ro in rs_owners {
                            if ro.kind == "Deployment" {
                                let dapi: Api<Deployment> =
                                    Api::namespaced(client.clone(), namespace);
                                if let Some(d) = dapi.get_opt(&ro.name).await? {
                                    return Ok(d.metadata.annotations);
                                }
                            }
                        }
                    }
                    return Ok(rs.metadata.annotations);
                }
            }
            "StatefulSet" => {
                let api: Api<StatefulSet> = Api::namespaced(client.clone(), namespace);
                if let Some(r) = api.get_opt(&owner.name).await? {
                    return Ok(r.metadata.annotations);
                }
            }
            "DaemonSet" => {
                let api: Api<DaemonSet> = Api::namespaced(client.clone(), namespace);
                if let Some(r) = api.get_opt(&owner.name).await? {
                    return Ok(r.metadata.annotations);
                }
            }
            "Job" => {
                let api: Api<Job> = Api::namespaced(client.clone(), namespace);
                if let Some(r) = api.get_opt(&owner.name).await? {
                    return Ok(r.metadata.annotations);
                }
            }
            _ => {}
        }
    }
    Ok(None)
}

pub async fn namespace_annotations(
    client: &kube::Client,
    ns: &str,
) -> Result<Option<BTreeMap<String, String>>> {
    let api: Api<Namespace> = Api::all(client.clone());
    Ok(api.get_opt(ns).await?.and_then(|n| n.metadata.annotations))
}

pub async fn service_account_annotations(
    client: &kube::Client,
    namespace: &str,
    sa: &str,
) -> Result<Option<BTreeMap<String, String>>> {
    let api: Api<ServiceAccount> = Api::namespaced(client.clone(), namespace);
    Ok(api.get_opt(sa).await?.and_then(|s| s.metadata.annotations))
}

/// Server-side-apply a single-key ConfigMap. Idempotent: safe to call on every admission, the API
/// server deduplicates by name + content.
pub async fn upsert_configmap(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    data_key: &str,
    data_value: &str,
) -> Result<()> {
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);

    let mut data = BTreeMap::new();
    data.insert(data_key.to_string(), data_value.to_string());

    let mut labels = BTreeMap::new();
    labels.insert(
        "app.kubernetes.io/managed-by".to_string(),
        MANAGER.to_string(),
    );

    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            labels: Some(labels),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    };

    // No `.force()`: cwii owns this field manager, so its own re-applies never conflict, while a
    // collision with a foreign manager surfaces as an error instead of being silently overwritten.
    let params = PatchParams::apply(MANAGER);
    api.patch(name, &params, &Patch::Apply(&cm))
        .await
        .with_context(|| format!("apply ConfigMap {namespace}/{name}"))?;
    Ok(())
}
