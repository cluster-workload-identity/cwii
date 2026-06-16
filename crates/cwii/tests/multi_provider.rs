//! End-to-end test that all three providers, enabled together on one pod, each receive their own
//! projected ServiceAccount token with the correct audience and mount path — the core multi-cloud
//! guarantee.

use std::collections::BTreeMap;

use cwii_core::{AnnotationSet, Provider, ProviderContext, merge, patch};
use cwii_provider_aws::{AwsConfig, AwsProvider};
use cwii_provider_az::{AzConfig, AzProvider};
use cwii_provider_gcp::{GcpConfig, GcpProvider};
use k8s_openapi::api::core::v1::Pod;
use serde_json::{Value, json};

fn annotations() -> BTreeMap<String, String> {
    [
        ("cwii.dev/gcp-inject", "true"),
        ("cwii.dev/gcp-audience", "//iam.googleapis.com/x"),
        ("cwii.dev/aws-inject", "true"),
        (
            "cwii.dev/aws-role-arn",
            "arn:aws:iam::123456789012:role/app",
        ),
        ("cwii.dev/az-inject", "true"),
        (
            "cwii.dev/az-client-id",
            "11111111-1111-1111-1111-111111111111",
        ),
        (
            "cwii.dev/az-tenant-id",
            "22222222-2222-2222-2222-222222222222",
        ),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
    .collect()
}

#[test]
fn all_three_providers_get_separate_tokens() {
    let anns = annotations();
    let aset = AnnotationSet {
        pod: Some(&anns),
        ..Default::default()
    };
    let ctx = ProviderContext {
        annotations: &aset,
        namespace: "demo",
        service_account_name: "default",
        mount_root: "/var/run/secrets/cwii.dev",
        default_token_expiration_secs: 3600,
    };

    let providers: Vec<Box<dyn Provider>> = vec![
        Box::new(GcpProvider::new(GcpConfig::default())),
        Box::new(AwsProvider::new(AwsConfig::default())),
        Box::new(AzProvider::new(AzConfig::default())),
    ];
    let plans: Vec<_> = providers
        .iter()
        .map(|p| {
            p.plan(&ctx)
                .unwrap()
                .expect("provider should produce a plan")
        })
        .collect();
    let merged = merge(plans);

    // Three distinct provider plans recorded.
    assert_eq!(merged.injected_providers.len(), 3);

    // Each provider contributes its own token volume with the correct audience.
    let audience = |name: &str| -> String {
        merged
            .volumes
            .iter()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("missing token volume {name}"))
            .value["projected"]["sources"][0]["serviceAccountToken"]["audience"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(audience("cwii-gcp-token"), "//iam.googleapis.com/x");
    assert_eq!(audience("cwii-aws-token"), "sts.amazonaws.com");
    assert_eq!(audience("cwii-az-token"), "api://AzureADTokenExchange");

    // Tokens mount at distinct, provider-scoped paths.
    let mount_path = |name: &str| -> String {
        merged
            .container_mounts
            .iter()
            .find(|m| m.name == name)
            .unwrap()
            .mount_path
            .clone()
    };
    assert_eq!(
        mount_path("cwii-gcp-token"),
        "/var/run/secrets/cwii.dev/gcp"
    );
    assert_eq!(
        mount_path("cwii-aws-token"),
        "/var/run/secrets/cwii.dev/aws"
    );
    assert_eq!(mount_path("cwii-az-token"), "/var/run/secrets/cwii.dev/az");

    // The rendered patch records all three in the status marker, sorted.
    let pod: Pod = serde_json::from_value(
        json!({ "spec": { "containers": [{ "name": "app", "image": "x" }] } }),
    )
    .unwrap();
    let patches: Vec<Value> = patch::build(&pod, &merged);
    let marker = patches
        .iter()
        .find(|p| p["path"] == "/metadata/annotations")
        .unwrap();
    assert_eq!(marker["value"]["cwii.dev/injected"], "aws,az,gcp");
}
