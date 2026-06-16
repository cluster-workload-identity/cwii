# cwii — Cluster Workload Identity Injector

**Let pods on a self-hosted Kubernetes cluster authenticate to GCP, AWS and Azure using their
Kubernetes ServiceAccount tokens — no static keys, no secret rotation.**

cwii is a Rust mutating admission webhook. When a pod opts in via `cwii.dev/*` annotations, cwii
injects the per-cloud plumbing that off-the-shelf cloud SDKs already understand — a projected
ServiceAccount token with the right audience for each cloud, plus that cloud's credentials file or
environment variables.

## Supported providers

| Provider | Mechanism | What cwii injects |
| --- | --- | --- |
| GCP (`gcp`) | Workload Identity Federation (`external_account`) | `credentials.json` + `GOOGLE_APPLICATION_CREDENTIALS` |
| AWS (`aws`) | `AssumeRoleWithWebIdentity` | `AWS_ROLE_ARN`, `AWS_WEB_IDENTITY_TOKEN_FILE` (+ region/session) |
| Azure (`az`) | Entra ID federated identity credential | `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_FEDERATED_TOKEN_FILE` |

Each enabled provider gets its **own** projected token at
`/var/run/secrets/cwii.dev/<provider>/token`, because each cloud requires a token with a different
audience.

## Where to start

1. **[Self-hosted OIDC setup](self-hosted-oidc.md)** — make your cluster an OIDC identity provider
   the clouds trust. This is the prerequisite for everything else.
2. Per-cloud trust: **[GCP](gcp-setup.md)** · **[AWS](aws-setup.md)** · **[Azure](az-setup.md)**.
3. **[Install](install.md)** the webhook with Helm.
4. **[Annotation reference](annotations.md)** to opt workloads in.
5. **[Verification](verification.md)** to confirm federation works at pod start.

For internals and design, see **[Architecture](architecture.md)**; for the release flow, see
**[Releasing](releasing.md)**.

## Quickstart

!!! warning "Prerequisite"
    Your cluster must expose a publicly reachable HTTPS OIDC discovery document and JWKS so cloud STS
    endpoints can validate its tokens. See [Self-hosted OIDC setup](self-hosted-oidc.md) first.

```bash
helm install cwii oci://ghcr.io/cluster-workload-identity/charts/cwii \
  -n cwii-system --create-namespace

kubectl annotate namespace demo cwii.dev/aws-inject=true
```

```yaml
metadata:
  annotations:
    cwii.dev/aws-inject: "true"
    cwii.dev/aws-role-arn: "arn:aws:iam::123456789012:role/my-app"
```
