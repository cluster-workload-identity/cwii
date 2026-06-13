//! Turn a [`MutationPlan`] into idempotent RFC 6902 JSON patch operations against a pod.
//!
//! This is the only module aware of JSON-pointer syntax and the pod's current shape. It bootstraps
//! arrays that don't yet exist (a single `add` with the whole array, rather than appends to a
//! missing path), skips anything already present (so webhook reinvocation is a no-op), and writes
//! the `cwii.dev/injected` status marker.

use std::collections::BTreeSet;

use k8s_openapi::api::core::v1::Pod;
use serde_json::{Value, json};

use crate::annotations::K_INJECTED;
use crate::plan::{EnvVar, InitContainer, MutationPlan, VolumeMount, VolumeSpec};
use crate::provider::ProviderId;

/// Build the patch operations for `plan` against `pod`.
pub fn build(pod: &Pod, plan: &MutationPlan) -> Vec<Value> {
    let mut patches = Vec::new();
    let spec = pod.spec.as_ref();

    // Volumes — skip any name already on the pod (duplicate volume names are rejected by the API).
    let existing_vols = names(spec.and_then(|s| s.volumes.as_ref()), |v| v.name.as_str());
    let new_vols: Vec<Value> = plan
        .volumes
        .iter()
        .filter(|v| !existing_vols.contains(v.name.as_str()))
        .map(|v: &VolumeSpec| v.value.clone())
        .collect();
    add_array(
        &mut patches,
        "/spec/volumes",
        spec.and_then(|s| s.volumes.as_ref()).is_some(),
        new_vols,
    );

    // Init containers — likewise skip names already present.
    let existing_init = names(spec.and_then(|s| s.init_containers.as_ref()), |c| {
        c.name.as_str()
    });
    let new_inits: Vec<Value> = plan
        .init_containers
        .iter()
        .filter(|c| !existing_init.contains(c.name.as_str()))
        .map(InitContainer::to_value)
        .collect();
    add_array(
        &mut patches,
        "/spec/initContainers",
        spec.and_then(|s| s.init_containers.as_ref()).is_some(),
        new_inits,
    );

    // Per main container: mounts and env, each guarded against what the container already declares.
    if let Some(spec) = spec {
        for (i, c) in spec.containers.iter().enumerate() {
            let existing_mounts = names(c.volume_mounts.as_ref(), |m| m.name.as_str());
            let new_mounts: Vec<Value> = plan
                .container_mounts
                .iter()
                .filter(|m| !existing_mounts.contains(m.name.as_str()))
                .map(VolumeMount::to_value)
                .collect();
            add_array(
                &mut patches,
                &format!("/spec/containers/{i}/volumeMounts"),
                c.volume_mounts.is_some(),
                new_mounts,
            );

            let existing_env = names(c.env.as_ref(), |e| e.name.as_str());
            let new_env: Vec<Value> = plan
                .container_env
                .iter()
                .filter(|e| !existing_env.contains(e.name.as_str()))
                .map(EnvVar::to_value)
                .collect();
            add_array(
                &mut patches,
                &format!("/spec/containers/{i}/env"),
                c.env.is_some(),
                new_env,
            );
        }
    }

    // Status marker. `~1` is the JSON-pointer escape for `/` in `cwii.dev/injected`.
    let marker = merged_marker(pod, &plan.injected_providers);
    if pod.metadata.annotations.is_some() {
        patches.push(json!({
            "op": "add",
            "path": "/metadata/annotations/cwii.dev~1injected",
            "value": marker,
        }));
    } else {
        let mut anns = serde_json::Map::new();
        anns.insert(K_INJECTED.to_string(), Value::String(marker));
        patches.push(json!({
            "op": "add",
            "path": "/metadata/annotations",
            "value": Value::Object(anns),
        }));
    }

    patches
}

fn names<T>(list: Option<&Vec<T>>, key: impl Fn(&T) -> &str) -> BTreeSet<&str> {
    list.map(|items| items.iter().map(&key).collect())
        .unwrap_or_default()
}

/// Append items to an array, bootstrapping it with a single `add` of the whole array when the path
/// does not yet exist (a bare `add /path/-` against a missing array is invalid).
fn add_array(patches: &mut Vec<Value>, path: &str, exists: bool, items: Vec<Value>) {
    if items.is_empty() {
        return;
    }
    if exists {
        for item in items {
            patches.push(json!({ "op": "add", "path": format!("{path}/-"), "value": item }));
        }
    } else {
        patches.push(json!({ "op": "add", "path": path, "value": items }));
    }
}

/// Union the existing `cwii.dev/injected` marker with the newly injected providers, sorted.
fn merged_marker(pod: &Pod, newly: &[ProviderId]) -> String {
    let mut set: BTreeSet<String> = BTreeSet::new();
    if let Some(existing) = pod
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(K_INJECTED))
    {
        for p in existing.split(',') {
            let p = p.trim();
            if !p.is_empty() {
                set.insert(p.to_string());
            }
        }
    }
    for id in newly {
        set.insert(id.abbr().to_string());
    }
    set.into_iter().collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plan::{EnvVar, MutationPlan, VolumeMount, VolumeSpec};

    fn pod(v: Value) -> Pod {
        serde_json::from_value(v).unwrap()
    }

    fn find<'a>(patches: &'a [Value], path: &str) -> Option<&'a Value> {
        patches.iter().find(|p| p["path"] == path)
    }

    fn sample_plan() -> MutationPlan {
        MutationPlan {
            volumes: vec![VolumeSpec {
                name: "cwii-gcp-token".into(),
                value: json!({ "name": "cwii-gcp-token", "projected": {} }),
            }],
            container_mounts: vec![VolumeMount {
                name: "cwii-gcp-token".into(),
                mount_path: "/var/run/secrets/cwii.dev/gcp".into(),
                read_only: true,
            }],
            container_env: vec![EnvVar {
                name: "GOOGLE_APPLICATION_CREDENTIALS".into(),
                value: "/var/run/secrets/cwii.dev/gcp-creds/credentials.json".into(),
            }],
            init_containers: vec![],
            configmap_upserts: vec![],
            injected_providers: vec![ProviderId::Gcp],
        }
    }

    #[test]
    fn bootstraps_absent_arrays_with_full_array() {
        let p = pod(json!({ "spec": { "containers": [{ "name": "app", "image": "x" }] } }));
        let patches = build(&p, &sample_plan());

        let vols = find(&patches, "/spec/volumes").unwrap();
        assert!(vols["value"].is_array());
        assert_eq!(vols["value"].as_array().unwrap().len(), 1);

        let mounts = find(&patches, "/spec/containers/0/volumeMounts").unwrap();
        assert!(mounts["value"].is_array());

        let env = find(&patches, "/spec/containers/0/env").unwrap();
        assert!(env["value"].is_array());

        // No annotations on the pod -> create the whole annotations object.
        let marker = find(&patches, "/metadata/annotations").unwrap();
        assert_eq!(marker["value"]["cwii.dev/injected"], "gcp");
    }

    #[test]
    fn appends_to_existing_arrays() {
        let p = pod(json!({
            "spec": {
                "volumes": [{ "name": "existing", "emptyDir": {} }],
                "containers": [{
                    "name": "app", "image": "x",
                    "env": [{ "name": "FOO", "value": "bar" }]
                }]
            }
        }));
        let patches = build(&p, &sample_plan());
        assert!(find(&patches, "/spec/volumes/-").is_some());
        assert!(find(&patches, "/spec/containers/0/env/-").is_some());
    }

    #[test]
    fn idempotent_against_already_injected_pod() {
        let p = pod(json!({
            "metadata": { "annotations": { "cwii.dev/injected": "gcp" } },
            "spec": {
                "volumes": [{ "name": "cwii-gcp-token", "projected": {} }],
                "containers": [{
                    "name": "app", "image": "x",
                    "volumeMounts": [{ "name": "cwii-gcp-token", "mountPath": "/x" }],
                    "env": [{ "name": "GOOGLE_APPLICATION_CREDENTIALS", "value": "/x" }]
                }]
            }
        }));
        let patches = build(&p, &sample_plan());
        // Everything is already present -> only the marker write remains.
        assert!(find(&patches, "/spec/volumes/-").is_none());
        assert!(find(&patches, "/spec/containers/0/env/-").is_none());
        let marker = find(&patches, "/metadata/annotations/cwii.dev~1injected").unwrap();
        assert_eq!(marker["value"], "gcp");
    }

    #[test]
    fn marker_merges_and_sorts_providers() {
        let p = pod(json!({
            "metadata": { "annotations": { "cwii.dev/injected": "gcp" } },
            "spec": { "containers": [{ "name": "app", "image": "x" }] }
        }));
        let mut plan = sample_plan();
        plan.injected_providers = vec![ProviderId::Aws];
        let patches = build(&p, &plan);
        let marker = find(&patches, "/metadata/annotations/cwii.dev~1injected").unwrap();
        assert_eq!(marker["value"], "aws,gcp");
    }
}
