# Annotation reference

cwii (Cluster Workload Identity Injector) is a Rust Kubernetes mutating admission
webhook that lets pods on **self-hosted** clusters authenticate to GCP, AWS and
Azure using their Kubernetes ServiceAccount tokens — workload identity federation
with **no static keys**. You drive the injector almost entirely through
`cwii.dev/*` annotations on your pods and the objects that own or scope them.

This page is the complete reference for every annotation cwii reads or writes,
the precedence model that resolves them, and a set of copy-pasteable recipes.

> [!NOTE]
> Annotations only take effect once the webhook is installed and your cluster
> publishes a valid OIDC discovery document. See [install](./install.md) and the
> hard prerequisite in [self-hosted OIDC](./self-hosted-oidc.md). For the
> opt-in `can-i` checks, see [verification](./verification.md).

Throughout this document the provider abbreviations are:

| Provider | Abbr. (`<p>`) | Mechanism |
| --- | --- | --- |
| Google Cloud | `gcp` | external_account credentials (direct federation or impersonation) |
| Amazon Web Services | `aws` | `AssumeRoleWithWebIdentity` |
| Microsoft Azure | `az` | Entra ID federated identity credential |

---

## Complete annotation table

All annotation keys use the `cwii.dev/` prefix and are reproduced verbatim below.
Every key except `cwii.dev/injected` is an **input** you set; `cwii.dev/injected`
is an **output** the webhook writes onto the mutated pod.

### Per-provider toggles and shared knobs

These keys exist for each of the three providers. Substitute `<p>` with `gcp`,
`aws` or `az`.

| Annotation | Provider | Type | Default | Meaning |
| --- | --- | --- | --- | --- |
| `cwii.dev/gcp-inject` | gcp | `"true"` / `"false"` | unset (no injection) | Enable GCP injection for the pod. |
| `cwii.dev/aws-inject` | aws | `"true"` / `"false"` | unset (no injection) | Enable AWS injection for the pod. Also requires `cwii.dev/aws-role-arn`. |
| `cwii.dev/az-inject` | az | `"true"` / `"false"` | unset (no injection) | Enable Azure injection for the pod. Also requires `cwii.dev/az-client-id` and `cwii.dev/az-tenant-id`. |
| `cwii.dev/gcp-audience` | gcp | string | server `--gcp-default-audience` | Override the projected-token audience for GCP. |
| `cwii.dev/aws-audience` | aws | string | `sts.amazonaws.com` | Override the projected-token audience for AWS. |
| `cwii.dev/az-audience` | az | string | `api://AzureADTokenExchange` | Override the projected-token audience for Azure. |
| `cwii.dev/gcp-token-expiration` | gcp | integer (seconds) | `3600` (Kubernetes min `600`) | Projected token lifetime for GCP. |
| `cwii.dev/aws-token-expiration` | aws | integer (seconds) | `3600` (Kubernetes min `600`) | Projected token lifetime for AWS. |
| `cwii.dev/az-token-expiration` | az | integer (seconds) | `3600` (Kubernetes min `600`) | Projected token lifetime for Azure. |
| `cwii.dev/gcp-verify` | gcp | `"true"` / `"false"` | `false` | Add a non-blocking `can-i` init container for GCP. |
| `cwii.dev/aws-verify` | aws | `"true"` / `"false"` | `false` | Add a non-blocking `can-i` init container for AWS. |
| `cwii.dev/az-verify` | az | `"true"` / `"false"` | `false` | Add a non-blocking `can-i` init container for Azure. |
| `cwii.dev/gcp-verify-enforce` | gcp | `"true"` / `"false"` | `false` | Make a failed GCP verify block pod startup. |
| `cwii.dev/aws-verify-enforce` | aws | `"true"` / `"false"` | `false` | Make a failed AWS verify block pod startup. |
| `cwii.dev/az-verify-enforce` | az | `"true"` / `"false"` | `false` | Make a failed Azure verify block pod startup. |
| `cwii.dev/gcp-verify-image` | gcp | string (image ref) | server `--gcp-verify-image` (`google/cloud-sdk:slim`) | Override the GCP verify init-container image. |
| `cwii.dev/aws-verify-image` | aws | string (image ref) | server `--aws-verify-image` (`amazon/aws-cli:latest`) | Override the AWS verify init-container image. |
| `cwii.dev/az-verify-image` | az | string (image ref) | server `--az-verify-image` (`mcr.microsoft.com/azure-cli:latest`) | Override the Azure verify init-container image. |

### GCP-only annotations

| Annotation | Provider | Type | Default | Meaning |
| --- | --- | --- | --- | --- |
| `cwii.dev/gcp-service-account` | gcp | string (GSA email) | unset (direct federation) | Google service account email to impersonate. When set, cwii adds the impersonation URL to `credentials.json`; when unset, the pod federates **directly**. |
| `cwii.dev/gcp-delivery` | gcp | `config-map` / `init-container` | server `--gcp-delivery` | How `credentials.json` reaches the pod: a server-side-applied ConfigMap, or an init container that writes the file into an `emptyDir`. |

### AWS-only annotations

| Annotation | Provider | Type | Default | Meaning |
| --- | --- | --- | --- | --- |
| `cwii.dev/aws-role-arn` | aws | string (role ARN) | **none — REQUIRED to inject** | IAM role to assume. Sets `AWS_ROLE_ARN`. Without it, AWS injection does not happen. |
| `cwii.dev/aws-region` | aws | string | unset | Optional region. Sets `AWS_REGION`. |
| `cwii.dev/aws-role-session-name` | aws | string | unset | Optional session name. Sets `AWS_ROLE_SESSION_NAME`. |

### Azure-only annotations

| Annotation | Provider | Type | Default | Meaning |
| --- | --- | --- | --- | --- |
| `cwii.dev/az-client-id` | az | string (UUID) | **none — REQUIRED** | Entra ID application/client ID. Sets `AZURE_CLIENT_ID`. Required to inject. |
| `cwii.dev/az-tenant-id` | az | string (UUID) | **none — REQUIRED** | Entra ID tenant ID. Sets `AZURE_TENANT_ID`. Required to inject. |
| `cwii.dev/az-authority-host` | az | string (URL) | unset | Optional authority host. Sets `AZURE_AUTHORITY_HOST`. |

### Webhook-written status marker

| Annotation | Provider | Type | Default | Meaning |
| --- | --- | --- | --- | --- |
| `cwii.dev/injected` | — | string | — (written by webhook) | Comma-joined, **sorted** list of provider abbreviations that were injected, e.g. `aws,gcp`. Set by the webhook on the mutated pod; do not set it yourself. |

> [!TIP]
> `cwii.dev/injected` is the fastest way to confirm a mutation took effect:
> `kubectl get pod <name> -o jsonpath='{.metadata.annotations.cwii\.dev/injected}'`.

---

## What gets injected

Understanding the precedence model is easier once you know what each enabled
provider actually adds to the pod.

### Token mounting — the core multi-cloud design

Each enabled provider gets its **own** projected `serviceAccountToken` volume.
Providers mount separately because **each cloud needs a different token
audience**.

| Property | Value |
| --- | --- |
| Volume name | `cwii-<p>-token` |
| Mount path | `/var/run/secrets/cwii.dev/<p>` (read-only) |
| File | `token` |
| Token path | `/var/run/secrets/cwii.dev/<p>/token` |
| `expirationSeconds` | `3600` default, minimum `600` |

The projected tokens are standard OIDC JWTs whose `sub` claim is
`system:serviceaccount:<NS>:<SA>`, and whose `aud` is the provider audience
above (overridable per provider via `cwii.dev/<p>-audience`).

### GCP

cwii builds a Google `external_account` `credentials.json`:

```json
{
  "type": "external_account",
  "audience": "<gcp-audience>",
  "subject_token_type": "urn:ietf:params:oauth:token-type:jwt",
  "token_url": "https://sts.googleapis.com/v1/token",
  "token_info_url": "https://sts.googleapis.com/v1/introspect",
  "credential_source": {
    "file": "/var/run/secrets/cwii.dev/gcp/token"
  }
}
```

If `cwii.dev/gcp-service-account` is set, cwii additionally inserts:

```json
  "service_account_impersonation_url": "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/<GSA>:generateAccessToken"
```

Otherwise the pod uses **direct federation** (no impersonation URL).

The file is mounted at `/var/run/secrets/cwii.dev/gcp-creds/credentials.json`
and `GOOGLE_APPLICATION_CREDENTIALS` points there.

Delivery is controlled by `cwii.dev/gcp-delivery`:

| Delivery | How `credentials.json` is delivered | Cluster writes? |
| --- | --- | --- |
| `config-map` | Webhook server-side-applies a ConfigMap named `cwii-gcp-creds-<6 hex of sha256(audience+NUL+sa)>`, labeled `app.kubernetes.io/managed-by=cwii`, mounted via a configMap volume `cwii-gcp-creds`. | Yes — needs ConfigMap-write RBAC. |
| `init-container` | An `emptyDir` volume `cwii-gcp-creds` plus an init container `cwii-gcp-creds-writer` (image `busybox:stable`) that writes `credentials.json` from env `CWII_GCP_CREDS_JSON`. | No cluster writes. |

### AWS

AWS injection is **env-vars only** (no file beyond the projected token):

| Env var | Source / value |
| --- | --- |
| `AWS_ROLE_ARN` | `cwii.dev/aws-role-arn` |
| `AWS_WEB_IDENTITY_TOKEN_FILE` | `/var/run/secrets/cwii.dev/aws/token` |
| `AWS_REGION` | `cwii.dev/aws-region` (optional) |
| `AWS_ROLE_SESSION_NAME` | `cwii.dev/aws-role-session-name` (optional) |

Token audience defaults to `sts.amazonaws.com`. The mechanism is
`AssumeRoleWithWebIdentity`.

### Azure

Azure injection is **env-vars only**:

| Env var | Source / value |
| --- | --- |
| `AZURE_CLIENT_ID` | `cwii.dev/az-client-id` |
| `AZURE_TENANT_ID` | `cwii.dev/az-tenant-id` |
| `AZURE_FEDERATED_TOKEN_FILE` | `/var/run/secrets/cwii.dev/az/token` |
| `AZURE_AUTHORITY_HOST` | `cwii.dev/az-authority-host` (optional) |

Token audience defaults to `api://AzureADTokenExchange`. The mechanism is an
Entra ID federated identity credential.

### Verify (`can-i`) init containers

Opt-in via `cwii.dev/<p>-verify`. They run at order **10** (after the GCP
creds writer at order 0).

| Provider | Container | Command | Default image |
| --- | --- | --- | --- |
| gcp | `cwii-gcp-verify` | `gcloud auth application-default print-access-token` | `google/cloud-sdk:slim` |
| aws | `cwii-aws-verify` | `aws sts get-caller-identity` | `amazon/aws-cli:latest` |
| az | `cwii-az-verify` | `az login --service-principal ... --federated-token ... && az account show` | `mcr.microsoft.com/azure-cli:latest` |

By default the check is **non-blocking** — it is wrapped as
`<check> || echo ... >&2`, so it always exits 0 and only logs. With
`cwii.dev/<p>-verify-enforce` the check runs bare, so a non-zero exit blocks
pod startup. Override the image with `cwii.dev/<p>-verify-image` or the Helm
value `providers.<p>.verifyImage`. See [verification](./verification.md) for
details.

---

## Precedence model

cwii resolves each annotation from four scopes, in this order:

```text
pod  >  owning workload  >  ServiceAccount  >  namespace
```

Three rules make this predictable:

1. **Independent per key.** Each annotation key is resolved on its own. The
   audience for AWS coming from the namespace does not pin the inject toggle for
   GCP, and so on.
2. **First explicit value wins.** Walking from pod outward, the first scope that
   sets a key wins for that key. A more specific scope overrides a broader one.
3. **Specific `false` beats broader `true`.** Because the first explicit value
   wins, a pod-level `cwii.dev/gcp-inject: "false"` suppresses a namespace-level
   `cwii.dev/gcp-inject: "true"`. **One provider never affects another.**

### Owner walk

When resolving the "owning workload" scope, cwii walks owner references:
`ReplicaSet -> Deployment` (Deployment annotations are preferred over the
intermediate ReplicaSet), plus `StatefulSet`, `DaemonSet` and `Job`.

### Worked examples

**Example 1 — namespace opt-in, pod opt-out (specific `false` wins).**

| Scope | `cwii.dev/gcp-inject` |
| --- | --- |
| Namespace | `"true"` |
| Pod | `"false"` |

Result: GCP is **not** injected. The pod-level `false` is the first explicit
value walking outward and wins for the `gcp-inject` key.

**Example 2 — independent per key across providers and scopes.**

| Scope | `cwii.dev/gcp-inject` | `cwii.dev/aws-inject` | `cwii.dev/aws-role-arn` |
| --- | --- | --- | --- |
| Namespace | `"true"` | — | — |
| ServiceAccount | — | `"true"` | `arn:aws:iam::111122223333:role/data` |
| Pod | — | — | — |

Result: both GCP and AWS are injected. `gcp-inject` resolves from the namespace;
`aws-inject` and `aws-role-arn` resolve independently from the ServiceAccount.
`cwii.dev/injected` becomes `aws,gcp` (sorted).

**Example 3 — Deployment preferred over ReplicaSet, overridden by pod.**

| Scope | `cwii.dev/aws-audience` |
| --- | --- |
| Deployment | `sts.amazonaws.com` |
| Pod template (becomes the Pod) | `sts.eu-west-1.amazonaws.com` |

Result: the audience is `sts.eu-west-1.amazonaws.com`. The pod's own annotation
wins. (Between a ReplicaSet and its Deployment, the Deployment's value would
win.)

---

## Recipes

### Namespace-wide GCP opt-in

Turn on GCP federation for everything in a namespace, then bind the
ServiceAccount in GCP as a federated principal.

```bash
kubectl annotate namespace team-analytics cwii.dev/gcp-inject=true
```

```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: team-analytics
  annotations:
    cwii.dev/gcp-inject: "true"
    cwii.dev/gcp-audience: "//iam.googleapis.com/projects/123456789/locations/global/workloadIdentityPools/onprem/providers/k8s"
```

On the GCP side, grant the federated principal access (direct federation):

```bash
gcloud projects add-iam-policy-binding my-project \
  --role roles/storage.objectViewer \
  --member "principal://iam.googleapis.com/projects/123456789/locations/global/workloadIdentityPools/onprem/subject/system:serviceaccount:team-analytics:default"
```

### Per-Deployment AWS with a role

`cwii.dev/aws-role-arn` is required for AWS injection. Set it (and the toggle)
on the Deployment so every pod inherits it via the owner walk.

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: ingest
  namespace: pipelines
  annotations:
    cwii.dev/aws-inject: "true"
    cwii.dev/aws-role-arn: "arn:aws:iam::111122223333:role/cwii-ingest"
    cwii.dev/aws-region: "eu-west-1"
spec:
  template:
    metadata: {}
    spec:
      containers:
        - name: app
          image: ghcr.io/example/ingest:latest
```

The matching IAM trust policy (OIDC provider already registered for your
cluster issuer):

```json
{
  "Version": "2012-10-17",
  "Statement": [{
    "Effect": "Allow",
    "Principal": { "Federated": "arn:aws:iam::111122223333:oidc-provider/oidc.example.com" },
    "Action": "sts:AssumeRoleWithWebIdentity",
    "Condition": {
      "StringEquals": {
        "oidc.example.com:aud": "sts.amazonaws.com",
        "oidc.example.com:sub": "system:serviceaccount:pipelines:default"
      }
    }
  }]
}
```

### Dual / triple-provider pod

Each enabled provider gets its own token volume and its own audience, so a
single pod can talk to all three clouds at once.

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: multi-cloud
  namespace: workloads
  annotations:
    cwii.dev/gcp-inject: "true"
    cwii.dev/gcp-service-account: "data-reader@my-project.iam.gserviceaccount.com"
    cwii.dev/aws-inject: "true"
    cwii.dev/aws-role-arn: "arn:aws:iam::111122223333:role/cwii-multi"
    cwii.dev/az-inject: "true"
    cwii.dev/az-client-id: "00000000-0000-0000-0000-000000000000"
    cwii.dev/az-tenant-id: "11111111-1111-1111-1111-111111111111"
spec:
  containers:
    - name: app
      image: ghcr.io/example/multi:latest
```

After admission, `cwii.dev/injected` on the pod will read `aws,az,gcp`.
The pod ends up with three projected token volumes
(`cwii-gcp-token`, `cwii-aws-token`, `cwii-az-token`) mounted under
`/var/run/secrets/cwii.dev/{gcp,aws,az}`.

### Pod opt-out within an opted-in namespace

Use a specific `false` to suppress a broader `true` for a single provider,
without affecting the others.

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: no-gcp-here
  namespace: team-analytics      # namespace sets cwii.dev/gcp-inject: "true"
  annotations:
    cwii.dev/gcp-inject: "false" # this pod opts out of GCP only
spec:
  containers:
    - name: app
      image: ghcr.io/example/app:latest
```

### GCP impersonation

Set `cwii.dev/gcp-service-account` to a GSA email. cwii adds the impersonation
URL to `credentials.json` instead of federating directly.

```yaml
metadata:
  annotations:
    cwii.dev/gcp-inject: "true"
    cwii.dev/gcp-service-account: "data-reader@my-project.iam.gserviceaccount.com"
```

Allow the federated principal to impersonate the GSA:

```bash
gcloud iam service-accounts add-iam-policy-binding \
  data-reader@my-project.iam.gserviceaccount.com \
  --role roles/iam.workloadIdentityUser \
  --member "principal://iam.googleapis.com/projects/123456789/locations/global/workloadIdentityPools/onprem/subject/system:serviceaccount:workloads:default"
```

### Init-container delivery for GCP credentials

Avoid granting the webhook ConfigMap-write RBAC by delivering `credentials.json`
through an init container into an `emptyDir`.

```yaml
metadata:
  annotations:
    cwii.dev/gcp-inject: "true"
    cwii.dev/gcp-delivery: "init-container"
```

This injects an `emptyDir` volume `cwii-gcp-creds` and an init container
`cwii-gcp-creds-writer` (image `busybox:stable`) that writes `credentials.json`
from the env var `CWII_GCP_CREDS_JSON`. No cluster writes occur, so the
ConfigMap RBAC rule is unnecessary. (With `config-map` delivery the webhook
needs `ConfigMap` `get/create/update/patch`; see [install](./install.md).)

### Enforced verify

Make a failed `can-i` check block pod startup so misconfigured federation fails
fast rather than at first API call.

```yaml
metadata:
  annotations:
    cwii.dev/aws-inject: "true"
    cwii.dev/aws-role-arn: "arn:aws:iam::111122223333:role/cwii-ingest"
    cwii.dev/aws-verify: "true"
    cwii.dev/aws-verify-enforce: "true"
```

With `aws-verify-enforce: "true"`, the `cwii-aws-verify` init container runs
`aws sts get-caller-identity` **bare** — a non-zero exit blocks the pod. Without
enforce, the same check is wrapped to always exit 0 and only logs. You can pin
the image per pod:

```yaml
    cwii.dev/aws-verify-image: "amazon/aws-cli:2.17.0"
```

---

## Native (managed-platform) annotation compatibility

For drop-in migration of workloads moving **off** GKE/EKS/AKS to a self-hosted cluster, cwii can
optionally read the managed platforms' own identity annotations as a **fallback**. This is **off by
default** (explicit opt-in is the safe posture) and enabled cluster-wide with the Helm value
`compat.nativeAnnotations: true` (flag `--native-annotations`).

When enabled, **the presence of a native annotation also triggers injection** for that provider
(mirroring how the managed platform behaves), and the native value is used when no `cwii.dev/*` one
is set. `cwii.dev/*` annotations **always take precedence**.

| Native annotation | Platform | Maps to | Notes |
| --- | --- | --- | --- |
| `iam.gke.io/gcp-service-account` | GKE | `cwii.dev/gcp-service-account` (impersonation) | GKE's annotation carries no audience, so you must also set a default `gcp-audience` (Helm `providers.gcp.defaultAudience`), else GCP injection is skipped. |
| `eks.amazonaws.com/role-arn` | EKS | `cwii.dev/aws-role-arn` | Audience defaults to `sts.amazonaws.com` — clean. |
| `azure.workload.identity/client-id` | AKS | `cwii.dev/az-client-id` | Enable signal for Azure when compat is on. |
| `azure.workload.identity/tenant-id` | AKS | `cwii.dev/az-tenant-id` | |

!!! warning
    Leaving this on means any pod/ServiceAccount carrying one of these annotations (e.g. imported
    manifests) will be injected. Enable it deliberately, ideally alongside a `namespaceSelector`
    scoping the webhook to the namespaces you're migrating.

## See also

- [Self-hosted OIDC setup](./self-hosted-oidc.md) — the hard prerequisite: your
  kube-apiserver must publish an HTTPS OIDC discovery document and JWKS the cloud
  STS endpoints can fetch.
- [Verification](./verification.md) — the opt-in `can-i` init containers in depth.
- [Install](./install.md) — Helm chart, RBAC, TLS, and webhook configuration.
