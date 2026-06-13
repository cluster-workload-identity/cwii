# cwii — Cluster Workload Identity Injector

[![CI](https://github.com/cluster-workload-identity/cwii/actions/workflows/ci.yaml/badge.svg)](https://github.com/cluster-workload-identity/cwii/actions/workflows/ci.yaml)
[![Release](https://github.com/cluster-workload-identity/cwii/actions/workflows/release-please.yaml/badge.svg)](https://github.com/cluster-workload-identity/cwii/actions/workflows/release-please.yaml)
[![codecov](https://codecov.io/gh/cluster-workload-identity/cwii/branch/main/graph/badge.svg)](https://codecov.io/gh/cluster-workload-identity/cwii)
[![Latest release](https://img.shields.io/github/v/release/cluster-workload-identity/cwii?sort=semver)](https://github.com/cluster-workload-identity/cwii/releases)
[![Image](https://img.shields.io/badge/image-ghcr.io%2Fcluster-workload-identity%2Fcwii-blue?logo=docker)](https://github.com/cluster-workload-identity/cwii/pkgs/container/cwii)
[![Docs](https://img.shields.io/badge/docs-cwii.dev-3f51b5)](https://cwii.dev)
[![License](https://img.shields.io/github/license/cluster-workload-identity/cwii)](./LICENSE)

**Let pods on a self-hosted Kubernetes cluster authenticate to GCP, AWS and Azure using their
Kubernetes ServiceAccount tokens — no static keys, no secret rotation.**

cwii is a Rust mutating admission webhook. When a pod opts in via annotations, cwii injects the
per-cloud plumbing that off-the-shelf cloud SDKs already understand, then gets out of the way.

## The problem

Managed clusters (GKE/EKS/AKS) have built-in workload identity. **Self-hosted** clusters don't, so
teams fall back to long-lived service-account keys and IAM access keys mounted as Secrets — with all
the rotation burden, blast radius and exfiltration risk that implies.

cwii makes the **cluster itself an OIDC identity provider** that cloud STS endpoints trust, and
injects the small amount of per-provider configuration each SDK needs to exchange the pod's
ServiceAccount token for short-lived cloud credentials.

## Supported providers

| Provider | Status | Mechanism | What cwii injects |
| --- | --- | --- | --- |
| GCP (`gcp`) | ✅ GA | Workload Identity Federation (`external_account`) | `credentials.json` + `GOOGLE_APPLICATION_CREDENTIALS` |
| AWS (`aws`) | ✅ GA | `AssumeRoleWithWebIdentity` | `AWS_ROLE_ARN`, `AWS_WEB_IDENTITY_TOKEN_FILE` (+ region/session) |
| Azure (`az`) | ✅ GA | Entra ID federated identity credential | `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_FEDERATED_TOKEN_FILE` |

Adding a provider is a small, self-contained crate implementing one trait — see
[CONTRIBUTING.md](./CONTRIBUTING.md).

## How it works

```
 kube-apiserver (OIDC issuer + signing key)
        │  publishes /.well-known/openid-configuration + JWKS over public HTTPS
        ▼
   GCP / AWS / Azure STS  ◄── fetch JWKS, validate the projected token (aud, sub, exp)
        ▲
        │ SDK exchanges the token at pod runtime
 ┌──────┴────────────────────────────────────────────────────────┐
 │ pod (mutated by cwii), per enabled provider:                   │
 │  • a projected ServiceAccount token volume with that cloud's   │
 │    audience, at /var/run/secrets/cwii.dev/<provider>/token     │
 │  • GCP: credentials.json + GOOGLE_APPLICATION_CREDENTIALS      │
 │  • AWS/Azure: the provider's env vars                          │
 └────────────────────────────────────────────────────────────────┘
        ▲ pod CREATE
        │
 cwii MutatingWebhookConfiguration (mutate.cwii.dev)
```

Each enabled provider gets its **own** projected token, because each cloud requires a token with a
different audience — that's the core design choice that lets one pod federate to several clouds.

## Quickstart

> **Prerequisite — do this first:** your cluster must expose a publicly reachable HTTPS OIDC
> discovery document and JWKS so the cloud STS endpoints can validate its tokens. This is the
> hardest part; see **[docs/self-hosted-oidc.md](./docs/self-hosted-oidc.md)**.

```bash
# Install (cert-manager recommended; see docs/install.md for the self-signed fallback).
helm install cwii oci://ghcr.io/cluster-workload-identity/charts/cwii \
  -n cwii-system --create-namespace

# Opt a namespace into AWS injection.
kubectl annotate namespace demo cwii.dev/aws-inject=true
```

Annotate a workload:

```yaml
metadata:
  annotations:
    cwii.dev/aws-inject: "true"
    cwii.dev/aws-role-arn: "arn:aws:iam::123456789012:role/my-app"
    # add cwii.dev/aws-verify: "true" to run `aws sts get-caller-identity` at pod start
```

Then per-cloud IAM setup: **[GCP](./docs/gcp-setup.md)** · **[AWS](./docs/aws-setup.md)** ·
**[Azure](./docs/az-setup.md)**.

## Documentation

- **[Self-hosted OIDC setup](./docs/self-hosted-oidc.md)** — make your cluster an OIDC IdP (start here)
- **[GCP](./docs/gcp-setup.md)** · **[AWS](./docs/aws-setup.md)** · **[Azure](./docs/az-setup.md)** — per-cloud trust setup
- **[Installation](./docs/install.md)** — Helm values, cert-manager vs self-signed, `failurePolicy`
- **[Annotation reference](./docs/annotations.md)** — every `cwii.dev/*` key and the precedence model
- **[Verification](./docs/verification.md)** — the opt-in "can-i" init containers
- **[Architecture](./docs/architecture.md)** — internals, security posture, failure modes
- **[Releasing](./docs/releasing.md)** — automated SemVer + signed image/chart

Full site: **[cwii.dev](https://cwii.dev)**.

## Security

No static long-lived secrets; credentials are short-lived federated tokens. Images and charts are
multi-arch, distroless, non-root, and cosign-signed with SBOM + provenance. See
[SECURITY.md](./SECURITY.md).

## License

[Apache-2.0](./LICENSE).
