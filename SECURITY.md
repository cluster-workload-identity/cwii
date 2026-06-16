# Security Policy

> **cwii** (Cluster Workload Identity Injector) is a Rust Kubernetes mutating
> admission webhook that lets pods on **self-hosted** clusters authenticate to
> **GCP, AWS and Azure** using their Kubernetes ServiceAccount tokens (workload
> identity federation) — **no static keys**.
>
> Repo: `github.com/cluster-workload-identity/cwii` · Site: `cwii.dev` · Image:
> `ghcr.io/cluster-workload-identity/cwii` · Chart: `oci://ghcr.io/cluster-workload-identity/charts/cwii` ·
> Install namespace: `cwii-system`

This document describes cwii's security model, the threat model and its
mitigations, our supply-chain assurances, and how to report a vulnerability. It
is written for platform engineers running self-hosted Kubernetes.

For component-level detail, see the sibling docs:
[Installation](./install.md) ·
[Self-hosted OIDC prerequisites](./self-hosted-oidc.md) ·
[Annotations reference](./annotations.md) ·
[Verification (`can-i`) init containers](./verification.md)

---

## Table of contents

- [Reporting a vulnerability](#reporting-a-vulnerability)
- [Security model at a glance](#security-model-at-a-glance)
- [Threat model](#threat-model)
  - [Primary threat: a compromised webhook](#primary-threat-a-compromised-webhook)
  - [Mitigations](#mitigations)
- [No static long-lived secrets](#no-static-long-lived-secrets)
- [The admission webhook](#the-admission-webhook)
- [RBAC and least privilege](#rbac-and-least-privilege)
- [Scoping injection with selectors](#scoping-injection-with-selectors)
- [Cloud trust policies (constrain the subject)](#cloud-trust-policies-constrain-the-subject)
  - [GCP — Workload Identity Federation](#gcp--workload-identity-federation)
  - [AWS — AssumeRoleWithWebIdentity](#aws--assumerolewithwebidentity)
  - [Azure — Entra ID federated identity credential](#azure--entra-id-federated-identity-credential)
- [Token mounting and federation flow](#token-mounting-and-federation-flow)
- [Verification (`can-i`) init containers](#verification-can-i-init-containers)
- [Supply-chain security](#supply-chain-security)
- [Operator hardening checklist](#operator-hardening-checklist)

---

## Reporting a vulnerability

> [!IMPORTANT]
> **Do not open a public GitHub issue for security vulnerabilities.**

Please report suspected vulnerabilities privately via **GitHub Private Security
Advisories**:

1. Go to the cwii repository on GitHub: `github.com/cluster-workload-identity/cwii`.
2. Open the **Security** tab.
3. Choose **Report a vulnerability** to open a private advisory draft.

Include as much detail as you can — affected version(s), reproduction steps,
impact, and any suggested remediation.

**Response window.** We aim to acknowledge a report within **3 business days**
and to provide an initial assessment within **7 business days**. We will keep you
informed as we triage, develop a fix, and coordinate disclosure.

---

## Security model at a glance

| Property | cwii behaviour |
| --- | --- |
| Authentication mechanism | Workload identity federation — pods use their projected Kubernetes ServiceAccount tokens to obtain short-lived cloud credentials |
| Long-lived secrets | **None.** No cloud keys are stored or injected anywhere |
| Only sensitive material | The webhook's **TLS serving certificate** |
| Cloud credentials | Short-lived **federated tokens** (per-provider audiences, expiry default `3600s`, min `600s`) |
| Container image | Distroless **nonroot** (uid `65532`), multi-arch `amd64`+`arm64` |
| Webhook failure mode | `failurePolicy` default `Ignore`; set `Fail` to require injection |
| RBAC | Read-mostly ClusterRole; ConfigMap write **only** when GCP `configMap` delivery is enabled |

---

## Threat model

### Primary threat: a compromised webhook

cwii is a **mutating admission webhook**: at pod `CREATE` time it can add
volumes, volume mounts, environment variables, and init containers to workloads.

The principal risk is therefore that **a compromised or misconfigured webhook
could inject arbitrary cloud identity into pods** — for example mounting a
projected token with an attacker-chosen audience, or wiring environment
variables that point a pod at a role/identity it should not assume. Because
federation derives a pod's cloud identity from its Kubernetes ServiceAccount
token (`sub = system:serviceaccount:<NS>:<SA>`), the blast radius of an injection
is ultimately bounded by **what the cloud trust policy allows that subject to do**.

### Mitigations

The defence is **defence in depth across three layers**:

1. **Tight RBAC on the webhook.** Grant cwii only the permissions it needs (see
   [RBAC and least privilege](#rbac-and-least-privilege)). In particular, avoid
   ConfigMap write by preferring the `initContainer` GCP delivery mode.
2. **`namespaceSelector` / `objectSelector` scoping.** Limit which namespaces and
   objects the webhook is even consulted for (see
   [Scoping injection with selectors](#scoping-injection-with-selectors)).
3. **Least-privilege cloud trust policies.** This is the most important control:
   constrain the federated `sub` to **specific**
   `system:serviceaccount:<NS>:<SA>` values, restrict WIF allowed audiences, and
   use IAM trust conditions so that even a maximally-permissive injection cannot
   obtain credentials a workload should not have (see
   [Cloud trust policies](#cloud-trust-policies-constrain-the-subject)).

> [!TIP]
> Treat the cloud-side trust policy as the real security boundary. The webhook
> decides *how* a token is mounted; the cloud STS decides *who* that token is
> allowed to be.

---

## No static long-lived secrets

cwii deliberately avoids static cloud credentials end to end:

- **No service-account keys, no access keys, no client secrets** are stored,
  templated, or injected by cwii.
- The only sensitive material cwii itself holds is its **TLS serving
  certificate** (used to terminate the admission HTTPS endpoint).
- Workload credentials are **short-lived federated tokens**. Each enabled
  provider mounts a projected ServiceAccount token with a per-provider audience
  and a bounded lifetime (`expirationSeconds` default `3600`, Kubernetes minimum
  `600`).

This means there is no long-lived secret for an attacker to exfiltrate from a
pod and replay indefinitely.

---

## The admission webhook

cwii registers a `MutatingWebhookConfiguration` with webhook name
**`mutate.cwii.dev`**.

| Setting | Value | Notes |
| --- | --- | --- |
| Rules | pods, `CREATE` | Mutates pods at creation |
| `sideEffects` | `NoneOnDryRun` | |
| `failurePolicy` | `Ignore` (default) | Set `Fail` to **require** injection |
| `reinvocationPolicy` | `Never` | |
| `namespaceSelector` | Always excludes the release namespace, `kube-system`, and `kube-node-lease` | Deadlock-safety |

> [!WARNING]
> The `namespaceSelector` **always** excludes the release namespace,
> `kube-system`, and `kube-node-lease`. This is deadlock-safety and is
> **critical when `failurePolicy=Fail`** — otherwise a broken webhook could block
> creation of system pods (and the webhook's own pods) cluster-wide.

### TLS serving certificate

The webhook server terminates HTTPS. There are two supported ways to provision
the serving certificate:

- **cert-manager (default).** The chart creates a `Certificate` resource and uses
  the `cert-manager.io/inject-ca-from` annotation to inject the CA bundle. The
  **chart does not set `caBundle`** itself — cert-manager does.
- **Helm `genSignedCert` self-signed fallback** (`tls.certManager.enabled=false`).
  The chart templates the serving `Secret` and the `caBundle` together.

### Server flags / environment

The server is configured with `clap` flags (each has a matching environment
variable):

| Flag | Env var | Default |
| --- | --- | --- |
| `--addr` | `CWII_ADDR` | `0.0.0.0:8443` |
| `--tls-cert` | `CWII_TLS_CERT` | `/tls/tls.crt` |
| `--tls-key` | `CWII_TLS_KEY` | `/tls/tls.key` |
| `--mount-root` | `CWII_MOUNT_ROOT` | `/var/run/secrets/cwii.dev` |
| `--token-expiration` | `CWII_TOKEN_EXPIRATION` | `3600` |
| `--gcp-enabled` / `--aws-enabled` / `--az-enabled` | — | `true` |
| `--gcp-default-audience` | — | — |
| `--gcp-delivery` | — | `config-map` \| `init-container` |
| `--gcp-init-image` | — | — |
| `--gcp-verify-image` | — | — |
| `--aws-default-audience` | — | `sts.amazonaws.com` |
| `--aws-verify-image` | — | — |
| `--az-default-audience` | — | `api://AzureADTokenExchange` |
| `--az-verify-image` | — | — |

**Endpoints:** `GET /healthz`, `POST /mutate` (HTTPS).

The image is **distroless nonroot** (uid `65532`), multi-arch `amd64`+`arm64`.

---

## RBAC and least privilege

cwii ships a `ClusterRole` that is **read-mostly**. Write access is granted
**only** when GCP `configMap` delivery is enabled.

| API group | Resources | Verbs | When |
| --- | --- | --- | --- |
| core (`""`) | `namespaces`, `serviceaccounts` | `get`, `list`, `watch` | Always |
| `apps` | `deployments`, `statefulsets`, `daemonsets`, `replicasets` | `get`, `list`, `watch` | Always (owner walk) |
| `batch` | `jobs` | `get`, `list`, `watch` | Always (owner walk) |
| core (`""`) | `configmaps` | `get`, `create`, `update`, `patch` | **Only** when GCP `configMap` delivery is enabled |

> [!TIP]
> To keep the webhook's RBAC strictly read-only, prefer the GCP `initContainer`
> delivery mode (`cwii.dev/gcp-delivery: init-container`). That mode writes
> `credentials.json` inside the pod from an init container and performs **no
> cluster writes**, so the ConfigMap `create/update/patch` grant is never needed.

The read permissions exist because precedence resolution requires walking owner
references and reading annotations on the owning workload, the ServiceAccount,
and the namespace.

---

## Scoping injection with selectors

The webhook's blast radius is reduced by limiting which objects it is invoked
for. Use both `namespaceSelector` (already hardened to exclude system
namespaces) and `objectSelector`.

> [!NOTE]
> The release namespace, `kube-system`, and `kube-node-lease` are **always**
> excluded by cwii's `namespaceSelector` and cannot be re-included.

Example: only inject in namespaces explicitly opted in, and only for pods that
opt in via a label.

```yaml
webhooks:
  - name: mutate.cwii.dev
    failurePolicy: Fail # require injection in scoped namespaces
    namespaceSelector:
      matchLabels:
        cwii.dev/enabled: "true"
    objectSelector:
      matchLabels:
        cwii.dev/enabled: "true"
```

```bash
# Opt a namespace into cwii injection
kubectl label namespace team-payments cwii.dev/enabled=true
```

> [!TIP]
> Combining a narrow `namespaceSelector` with `failurePolicy: Fail` gives you
> strong injection guarantees **without** risking cluster-wide deadlock: the
> `Fail` policy only applies to the small set of namespaces you have opted in,
> and the system namespaces remain unconditionally excluded.

---

## Cloud trust policies (constrain the subject)

This is the most important security control. Projected ServiceAccount tokens are
standard OIDC JWTs whose subject is:

```text
sub = system:serviceaccount:<NAMESPACE>:<SERVICEACCOUNT>
```

Always pin your cloud-side trust to **exact** subjects (and audiences) rather
than wildcards. This ensures that even if an injection were misconfigured or
malicious, the cloud STS will only mint credentials for the precise workloads
you intended.

> The kube-apiserver must publish a publicly reachable OIDC discovery document
> and JWKS so the cloud STS endpoints can validate these tokens. See
> [Self-hosted OIDC prerequisites](./self-hosted-oidc.md) for the required
> `--service-account-issuer`, `--service-account-jwks-uri`,
> `--service-account-signing-key-file`, `--service-account-key-file`, and
> `--api-audiences` kube-apiserver flags.

### GCP — Workload Identity Federation

Create a workload identity pool/provider whose attribute mapping carries the
token subject, then **restrict the attribute condition to the exact subject**.

```bash
# Provider that maps the OIDC subject and restricts to one K8s ServiceAccount
gcloud iam workload-identity-pools providers create-oidc cwii-provider \
  --location="global" \
  --workload-identity-pool="cwii-pool" \
  --issuer-uri="https://oidc.example.com" \
  --allowed-audiences="//iam.googleapis.com/projects/123456789/locations/global/workloadIdentityPools/cwii-pool/providers/cwii-provider" \
  --attribute-mapping="google.subject=assertion.sub" \
  --attribute-condition="assertion.sub == 'system:serviceaccount:team-payments:checkout'"
```

- The `cwii.dev/gcp-audience` annotation (or `--gcp-default-audience`) must match
  the **allowed audience** configured above.
- **Direct federation** (no impersonation) is the default. If you set
  `cwii.dev/gcp-service-account` to a GSA email, cwii adds an impersonation URL;
  grant that GSA only the roles the workload needs, and grant the federated
  principal `roles/iam.workloadIdentityUser` on that GSA only.

```bash
# Allow only the exact subject to impersonate the GSA (when using impersonation)
gcloud iam service-accounts add-iam-policy-binding checkout@PROJECT.iam.gserviceaccount.com \
  --role="roles/iam.workloadIdentityUser" \
  --member="principal://iam.googleapis.com/projects/123456789/locations/global/workloadIdentityPools/cwii-pool/subject/system:serviceaccount:team-payments:checkout"
```

### AWS — AssumeRoleWithWebIdentity

Pin the IAM role trust policy to the exact subject (`sub`) and audience (`aud`)
using `StringEquals` conditions on the OIDC provider.

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Principal": {
        "Federated": "arn:aws:iam::123456789012:oidc-provider/oidc.example.com"
      },
      "Action": "sts:AssumeRoleWithWebIdentity",
      "Condition": {
        "StringEquals": {
          "oidc.example.com:aud": "sts.amazonaws.com",
          "oidc.example.com:sub": "system:serviceaccount:team-payments:checkout"
        }
      }
    }
  ]
}
```

```bash
# Register the cluster issuer as an IAM OIDC provider (once per cluster)
aws iam create-open-id-connect-provider \
  --url "https://oidc.example.com" \
  --client-id-list "sts.amazonaws.com"
```

- `cwii.dev/aws-role-arn` is **required** to inject for AWS.
- The default token audience is `sts.amazonaws.com` — keep the `aud` condition in
  sync with any `cwii.dev/aws-audience` override.

### Azure — Entra ID federated identity credential

Create a federated identity credential on the target app/user-assigned managed
identity whose `subject` is the exact Kubernetes ServiceAccount and whose
`audiences` match the projected token audience.

```bash
az identity federated-credential create \
  --name cwii-checkout \
  --identity-name checkout-mi \
  --resource-group team-payments-rg \
  --issuer "https://oidc.example.com" \
  --subject "system:serviceaccount:team-payments:checkout" \
  --audiences "api://AzureADTokenExchange"
```

- `cwii.dev/az-client-id` and `cwii.dev/az-tenant-id` are **required** to inject
  for Azure.
- The default token audience is `api://AzureADTokenExchange` — keep the
  `audiences` value in sync with any `cwii.dev/az-audience` override.

---

## Token mounting and federation flow

The core multi-cloud design is that **each enabled provider gets its own
projected ServiceAccount token volume**, because each cloud requires a
**different** token audience.

For each enabled provider `<p>` (`gcp`, `aws`, `az`):

| Item | Value |
| --- | --- |
| Volume name | `cwii-<p>-token` |
| Mount path (read-only) | `/var/run/secrets/cwii.dev/<p>` |
| Token file | `/var/run/secrets/cwii.dev/<p>/token` |
| `expirationSeconds` | default `3600`, min `600` |

**GCP** builds a Google `external_account` `credentials.json`:

| Field | Value |
| --- | --- |
| `type` | `external_account` |
| `audience` | `<gcp-audience>` |
| `subject_token_type` | `urn:ietf:params:oauth:token-type:jwt` |
| `token_url` | `https://sts.googleapis.com/v1/token` |
| `token_info_url` | `https://sts.googleapis.com/v1/introspect` |
| `credential_source.file` | `/var/run/secrets/cwii.dev/gcp/token` |
| `service_account_impersonation_url` | `https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/<GSA>:generateAccessToken` (only if `cwii.dev/gcp-service-account` set; direct federation otherwise) |

`credentials.json` is mounted at
`/var/run/secrets/cwii.dev/gcp-creds/credentials.json` and
`GOOGLE_APPLICATION_CREDENTIALS` points there. Delivery is one of:

- **`configMap`** — the webhook server-side-applies a ConfigMap named
  `cwii-gcp-creds-<6 hex of sha256(audience+NUL+sa)>`, labeled
  `app.kubernetes.io/managed-by=cwii`, mounted via a `configMap` volume
  `cwii-gcp-creds`. **Requires ConfigMap-write RBAC.**
- **`initContainer`** — an `emptyDir` volume `cwii-gcp-creds` plus an init
  container `cwii-gcp-creds-writer` (image `busybox:stable`) that writes
  `credentials.json` from env `CWII_GCP_CREDS_JSON`. **No cluster writes.**

**AWS** is **env-vars only** (no file). cwii injects:

| Env var | Value |
| --- | --- |
| `AWS_ROLE_ARN` | `<role-arn>` (from `cwii.dev/aws-role-arn`, required) |
| `AWS_WEB_IDENTITY_TOKEN_FILE` | `/var/run/secrets/cwii.dev/aws/token` |
| `AWS_REGION` | optional (`cwii.dev/aws-region`) |
| `AWS_ROLE_SESSION_NAME` | optional (`cwii.dev/aws-role-session-name`) |

Default token audience `sts.amazonaws.com`; mechanism
`AssumeRoleWithWebIdentity`.

**Azure** is **env-vars only**. cwii injects:

| Env var | Value |
| --- | --- |
| `AZURE_CLIENT_ID` | from `cwii.dev/az-client-id` (required) |
| `AZURE_TENANT_ID` | from `cwii.dev/az-tenant-id` (required) |
| `AZURE_FEDERATED_TOKEN_FILE` | `/var/run/secrets/cwii.dev/az/token` |
| `AZURE_AUTHORITY_HOST` | optional (`cwii.dev/az-authority-host`) |

Default token audience `api://AzureADTokenExchange`; mechanism Entra ID
federated identity credential.

After mutating, the webhook writes the status marker `cwii.dev/injected` as a
comma-joined sorted list of provider abbreviations (e.g. `"aws,gcp"`).

See [Annotations reference](./annotations.md) for the full annotation set and
[precedence rules](./annotations.md) (pod > owning workload > ServiceAccount >
namespace, evaluated independently per key).

---

## Verification (`can-i`) init containers

cwii can inject an opt-in **verification (`can-i`) init container** per provider
via `cwii.dev/<p>-verify: "true"`. These run after the GCP writer (`cwii-gcp-creds-writer`,
order `0`) at order `10`.

| Provider | Init container | Command | Image |
| --- | --- | --- | --- |
| `gcp` | `cwii-gcp-verify` | `gcloud auth application-default print-access-token` | `google/cloud-sdk:slim` |
| `aws` | `cwii-aws-verify` | `aws sts get-caller-identity` | `amazon/aws-cli:latest` |
| `az` | `cwii-az-verify` | `az login --service-principal ... --federated-token ... && az account show` | `mcr.microsoft.com/azure-cli:latest` |

- **Non-blocking by default.** The check is wrapped as
  `<check> || echo ... >&2`, so it always exits `0` (it only logs failures).
- **`cwii.dev/<p>-verify-enforce: "true"`** runs the check **bare**, so a
  non-zero exit **blocks pod startup**.
- Override the image via `cwii.dev/<p>-verify-image` or Helm
  `providers.<p>.verifyImage`.

> [!TIP]
> Enable `cwii.dev/<p>-verify-enforce` in **staging** to catch trust-policy
> misconfiguration before it reaches production, where it would otherwise surface
> as a silent runtime auth failure.

See [Verification](./verification.md) for full details.

---

## Supply-chain security

cwii's build and release pipeline is hardened against supply-chain attacks:

- **Distroless nonroot image** — runs as uid `65532`, no shell or package
  manager in the runtime image; multi-arch `amd64`+`arm64`.
- **Multi-stage, locked builds** — reproducible builds with locked dependencies.
- **Cosign-signed images and charts** — both `ghcr.io/cluster-workload-identity/cwii` and the
  chart `oci://ghcr.io/cluster-workload-identity/charts/cwii` are signed.
- **SBOM + provenance** — Software Bill of Materials and build provenance are
  published with releases.
- **Trivy scanning** — images are scanned for known vulnerabilities.
- **`cargo-deny`** — enforces advisory (vulnerability) and license policy on
  dependencies.
- **Dependabot** — automated dependency updates.

Verify a signed image before deploying:

```bash
cosign verify ghcr.io/cluster-workload-identity/cwii:latest \
  --certificate-identity-regexp "https://github.com/cluster-workload-identity/cwii/.*" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com"
```

```bash
# Verify the signed Helm chart (OCI)
cosign verify oci://ghcr.io/cluster-workload-identity/charts/cwii:latest \
  --certificate-identity-regexp "https://github.com/cluster-workload-identity/cwii/.*" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com"
```

---

## Operator hardening checklist

> [!IMPORTANT]
> Work through this checklist before running cwii in production.

- [ ] **Set `failurePolicy` deliberately.** Use `Fail` to *require* injection,
      but only in combination with a narrow `namespaceSelector`/`objectSelector`.
      The system namespaces are excluded automatically.
- [ ] **Scope with selectors.** Opt namespaces and pods in explicitly rather than
      mutating everything. See
      [Scoping injection with selectors](#scoping-injection-with-selectors).
- [ ] **Use the least-RBAC delivery mode.** Prefer GCP `initContainer` delivery
      (`cwii.dev/gcp-delivery: init-container`) to avoid granting the webhook
      ConfigMap write permission.
- [ ] **Restrict cloud trust to exact subjects.** Pin every WIF provider / IAM
      trust policy / Entra federated credential to the precise
      `system:serviceaccount:<NS>:<SA>` and the correct audience — never
      wildcards. See
      [Cloud trust policies](#cloud-trust-policies-constrain-the-subject).
- [ ] **Enable `verify-enforce` in staging.** Turn on
      `cwii.dev/<p>-verify-enforce` in pre-production to fail fast on
      misconfigured trust before it reaches production.
- [ ] **Protect the TLS serving cert.** It is the only sensitive material cwii
      holds; manage it with cert-manager where possible.
- [ ] **Keep audiences in sync.** Any `cwii.dev/<p>-audience` override must match
      the audience allowed by the corresponding cloud trust policy.
