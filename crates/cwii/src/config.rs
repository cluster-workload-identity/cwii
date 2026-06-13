//! Command-line / environment configuration for the webhook binary, and registry construction.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

use clap::Parser;
use cwii_core::{CoreConfig, Provider};
use cwii_provider_aws::{AwsConfig, AwsProvider};
use cwii_provider_az::{AzConfig, AzProvider};
use cwii_provider_gcp::{GcpConfig, GcpDelivery, GcpProvider};

#[derive(Clone, Debug, Parser)]
#[command(name = "cwii", about = "Cluster Workload Identity Injector")]
pub struct Config {
    /// Address the HTTPS webhook server listens on.
    #[arg(long, env = "CWII_ADDR", default_value = "0.0.0.0:8443")]
    pub addr: SocketAddr,

    #[arg(long, env = "CWII_TLS_CERT", default_value = "/tls/tls.crt")]
    pub tls_cert: PathBuf,

    #[arg(long, env = "CWII_TLS_KEY", default_value = "/tls/tls.key")]
    pub tls_key: PathBuf,

    /// Root directory for cwii-managed mounts inside injected pods.
    #[arg(
        long,
        env = "CWII_MOUNT_ROOT",
        default_value = "/var/run/secrets/cwii.dev"
    )]
    pub mount_root: String,

    /// Default projected-token lifetime (seconds) when a provider has no per-pod override.
    #[arg(long, env = "CWII_TOKEN_EXPIRATION", default_value_t = 3600)]
    pub token_expiration: i64,

    /// Also read managed-platform native identity annotations (GKE/EKS/AKS) as a fallback, so
    /// migrated workloads work unchanged. `cwii.dev/*` annotations always take precedence.
    #[arg(long, env = "CWII_NATIVE_ANNOTATIONS", default_value_t = false, action = clap::ArgAction::Set)]
    pub native_annotations: bool,

    /// Export OpenTelemetry traces + metrics over OTLP.
    #[arg(long, env = "CWII_OTEL_ENABLED", default_value_t = false, action = clap::ArgAction::Set)]
    pub otel_enabled: bool,

    /// OTLP/gRPC endpoint (e.g. http://otel-collector:4317). Defaults to the standard
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` environment variable when unset.
    #[arg(long, env = "CWII_OTEL_ENDPOINT")]
    pub otel_endpoint: Option<String>,

    // ---- GCP ----
    #[arg(long, env = "CWII_GCP_ENABLED", default_value_t = true, action = clap::ArgAction::Set)]
    pub gcp_enabled: bool,

    /// Default GCP WIF audience (overridable via `cwii.dev/gcp-audience`).
    #[arg(long, env = "CWII_GCP_DEFAULT_AUDIENCE")]
    pub gcp_default_audience: Option<String>,

    /// Default GCP credentials delivery: `config-map` or `init-container`.
    #[arg(long, env = "CWII_GCP_DELIVERY", default_value = "config-map")]
    pub gcp_delivery: String,

    #[arg(long, env = "CWII_GCP_INIT_IMAGE", default_value = "busybox:stable")]
    pub gcp_init_image: String,

    #[arg(
        long,
        env = "CWII_GCP_VERIFY_IMAGE",
        default_value = "google/cloud-sdk:slim"
    )]
    pub gcp_verify_image: String,

    // ---- AWS ----
    #[arg(long, env = "CWII_AWS_ENABLED", default_value_t = true, action = clap::ArgAction::Set)]
    pub aws_enabled: bool,

    #[arg(
        long,
        env = "CWII_AWS_DEFAULT_AUDIENCE",
        default_value = "sts.amazonaws.com"
    )]
    pub aws_default_audience: String,

    #[arg(
        long,
        env = "CWII_AWS_VERIFY_IMAGE",
        default_value = "amazon/aws-cli:latest"
    )]
    pub aws_verify_image: String,

    // ---- Azure ----
    #[arg(long, env = "CWII_AZ_ENABLED", default_value_t = true, action = clap::ArgAction::Set)]
    pub az_enabled: bool,

    #[arg(
        long,
        env = "CWII_AZ_DEFAULT_AUDIENCE",
        default_value = "api://AzureADTokenExchange"
    )]
    pub az_default_audience: String,

    #[arg(
        long,
        env = "CWII_AZ_VERIFY_IMAGE",
        default_value = "mcr.microsoft.com/azure-cli:latest"
    )]
    pub az_verify_image: String,
}

impl Config {
    pub fn core(&self) -> CoreConfig {
        CoreConfig {
            mount_root: self.mount_root.clone(),
            token_expiration_secs: self.token_expiration,
        }
    }

    /// Build the provider registry in fixed order (gcp, aws), including only enabled providers.
    pub fn providers(&self) -> anyhow::Result<Vec<Box<dyn Provider>>> {
        let mut providers: Vec<Box<dyn Provider>> = Vec::new();

        if self.gcp_enabled {
            let delivery =
                GcpDelivery::from_str(&self.gcp_delivery).map_err(|e| anyhow::anyhow!(e))?;
            providers.push(Box::new(GcpProvider::new(GcpConfig {
                default_audience: self.gcp_default_audience.clone(),
                delivery,
                init_image: self.gcp_init_image.clone(),
                verify_image: self.gcp_verify_image.clone(),
                native_annotations: self.native_annotations,
            })));
        }

        if self.aws_enabled {
            providers.push(Box::new(AwsProvider::new(AwsConfig {
                default_audience: self.aws_default_audience.clone(),
                verify_image: self.aws_verify_image.clone(),
                native_annotations: self.native_annotations,
            })));
        }

        if self.az_enabled {
            providers.push(Box::new(AzProvider::new(AzConfig {
                default_audience: self.az_default_audience.clone(),
                verify_image: self.az_verify_image.clone(),
                native_annotations: self.native_annotations,
            })));
        }

        Ok(providers)
    }
}
