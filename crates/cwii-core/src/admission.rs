//! Admission orchestration: resolve annotations, consult each provider, merge the plans, perform
//! side effects (honoring dry-run), and build the patch. Ported from the gwii demo and generalized
//! to multiple providers.

use std::time::Instant;

use k8s_openapi::api::core::v1::Pod;
use kube::core::DynamicObject;
use kube::core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview};
use opentelemetry::KeyValue;
use serde_json::Value;

use crate::config::CoreConfig;
use crate::error::Error;
use crate::provider::{Provider, ProviderContext};
use crate::resolve::AnnotationSet;
use crate::{k8s, patch, plan, telemetry};

/// State the admission flow needs from the host binary, kept abstract so `cwii-core` doesn't depend
/// on the concrete server type.
pub trait WebhookState: Send + Sync {
    fn client(&self) -> &kube::Client;
    fn providers(&self) -> &[Box<dyn Provider>];
    fn core(&self) -> &CoreConfig;
}

/// Handle one `AdmissionReview`, returning the response (with patch, or a denial on error).
#[tracing::instrument(skip_all)]
pub async fn mutate<S: WebhookState>(
    state: &S,
    review: AdmissionReview<Pod>,
) -> Result<AdmissionReview<DynamicObject>, Error> {
    let start = Instant::now();
    let req: AdmissionRequest<Pod> = match review.try_into() {
        Ok(r) => r,
        Err(e) => {
            record(start, "bad_request");
            return Err(Error::BadRequest(e.to_string()));
        }
    };

    let resp = AdmissionResponse::from(&req);

    let (response, outcome) = match handle(state, &req).await {
        Ok(patches) if patches.is_empty() => (resp, "skip"),
        Ok(patches) => {
            let patch_value = Value::Array(patches);
            match serde_json::from_value::<json_patch::Patch>(patch_value) {
                Ok(patch) => match resp.with_patch(patch) {
                    Ok(r) => (r, "inject"),
                    Err(e) => {
                        tracing::error!(error = %e, "failed to serialize patch");
                        let denied =
                            AdmissionResponse::from(&req).deny(format!("patch serialize: {e}"));
                        (denied, "patch_error")
                    }
                },
                Err(e) => {
                    tracing::error!(error = %e, "failed to parse patch as json-patch");
                    let denied = AdmissionResponse::from(&req).deny(format!("patch parse: {e}"));
                    (denied, "patch_error")
                }
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "mutation failed");
            (AdmissionResponse::from(&req).deny(e.to_string()), "deny")
        }
    };

    record(start, outcome);
    Ok(response.into_review())
}

/// Record the per-request metrics: an outcome-tagged counter and the handling duration.
fn record(start: Instant, outcome: &'static str) {
    let m = telemetry::metrics();
    m.requests.add(1, &[KeyValue::new("outcome", outcome)]);
    m.duration.record(start.elapsed().as_secs_f64(), &[]);
}

#[tracing::instrument(skip_all, fields(namespace))]
async fn handle<S: WebhookState>(
    state: &S,
    req: &AdmissionRequest<Pod>,
) -> anyhow::Result<Vec<Value>> {
    let pod = req
        .object
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("admission request has no object"))?;

    let namespace = req
        .namespace
        .as_deref()
        .or(pod.metadata.namespace.as_deref())
        .unwrap_or("default");

    let client = state.client();
    let pod_annos = pod.metadata.annotations.clone();
    let sa_name = pod
        .spec
        .as_ref()
        .and_then(|s| s.service_account_name.as_deref())
        .unwrap_or("default");

    // Fail closed: if a cluster read errors we cannot know the namespace/owner/SA-level policy, so
    // we deny rather than silently admit a pod without the identity it may be required to carry.
    // (A legitimately absent object is `Ok(None)`, so only real API errors deny.)
    let owner_annos = k8s::owner_annotations(client, namespace, pod).await?;
    let sa_annos = k8s::service_account_annotations(client, namespace, sa_name).await?;
    let ns_annos = k8s::namespace_annotations(client, namespace).await?;

    let annos = AnnotationSet {
        pod: pod_annos.as_ref(),
        owner: owner_annos.as_ref(),
        service_account: sa_annos.as_ref(),
        namespace: ns_annos.as_ref(),
    };

    let pod_display = pod
        .metadata
        .name
        .as_deref()
        .or(pod.metadata.generate_name.as_deref())
        .unwrap_or("<unknown>");

    let core = state.core();
    let ctx = ProviderContext {
        annotations: &annos,
        namespace,
        service_account_name: sa_name,
        mount_root: &core.mount_root,
        default_token_expiration_secs: core.token_expiration_secs,
    };

    // Every enabled provider is consulted on every admission. Reinvocation idempotency is handled
    // by the patch builder, which inspects the live pod spec — never the pod's own (untrusted)
    // `cwii.dev/injected` annotation.
    let mut plans = Vec::new();
    for provider in state.providers() {
        if !provider.enabled(&annos) {
            continue;
        }
        let abbr = provider.id().abbr();
        match provider.plan(&ctx) {
            Ok(Some(p)) => {
                tracing::info!(
                    namespace,
                    pod = pod_display,
                    provider = abbr,
                    "planned injection"
                );
                telemetry::metrics()
                    .injections
                    .add(1, &[KeyValue::new("provider", abbr)]);
                plans.push(p);
            }
            Ok(None) => {
                tracing::warn!(
                    namespace,
                    pod = pod_display,
                    provider = abbr,
                    "skip: provider enabled but not configured",
                );
            }
            Err(e) => return Err(e.context(format!("provider {abbr} failed to plan"))),
        }
    }

    if plans.is_empty() {
        tracing::debug!(namespace, pod = pod_display, "no providers to inject");
        return Ok(vec![]);
    }

    let merged = plan::merge(plans);

    for cm in &merged.configmap_upserts {
        if !req.dry_run {
            k8s::upsert_configmap(client, namespace, &cm.name, &cm.data_key, &cm.data_value)
                .await?;
            tracing::info!(namespace, configmap = %cm.name, "upserted credentials ConfigMap");
        } else {
            tracing::info!(namespace, configmap = %cm.name, "dry-run: skipping ConfigMap upsert");
        }
    }

    Ok(patch::build(pod, &merged))
}
