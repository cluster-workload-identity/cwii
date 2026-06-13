# GCP setup

This guide walks you through wiring [cwii](https://cwii.dev) (Cluster Workload Identity
Injector) into Google Cloud so that pods on your **self-hosted** Kubernetes cluster can
authenticate to GCP APIs using their Kubernetes ServiceAccount tokens — no static service
account keys, no key rotation, no secrets to leak.

At a high level you will:

1. Create a **Workload Identity Pool** and an **OIDC provider** that trusts your cluster's
   kube-apiserver as an identity provider.
2. Derive the **audience** string that cwii projects into each pod's token.
3. Grant access either by **direct federation** or by **service account impersonation**.
4. Annotate a workload so cwii injects a Google `external_account` credential.
5. (Optionally) verify the wiring end-to-end with the built-in `can-i` init container.

!!! abstract "Prerequisites"
    - **Your cluster issuer is published.** The kube-apiserver must expose a publicly
      reachable HTTPS OIDC discovery document (`/.well-known/openid-configuration`) and
      JWKS that Google's STS endpoints can fetch. If you have not done this yet, complete
      [Self-hosted OIDC setup](./self-hosted-oidc.md) **first** — nothing below will work
      without it.
    - **cwii is installed** in the `cwii-system` namespace with the GCP provider enabled
      (it is enabled by default). See [Install](./install.md).
    - **`gcloud` is authenticated** as a principal with permission to manage Workload
      Identity Pools and IAM (`roles/iam.workloadIdentityPoolAdmin` and
      `roles/iam.serviceAccountAdmin`, or equivalents), and the IAM, STS and IAM
      Credentials APIs are enabled on the project.

Set up some shell variables you will reuse throughout:

```bash
export PROJECT_ID="my-project"                         # human-readable project ID
export PROJECT_NUMBER="$(gcloud projects describe "$PROJECT_ID" --format='value(projectNumber)')"
export POOL="cwii-pool"                                 # Workload Identity Pool ID
export PROVIDER="cwii-cluster"                          # OIDC provider ID inside the pool
export ISSUER_URI="https://oidc.example.com/my-cluster" # == kube-apiserver --service-account-issuer
```

!!! warning "Use the project **number**, not the project ID"
    Workload Identity Pool resource names embed the numeric `projectNumber`
    (e.g. `123456789012`), **not** the alphanumeric `PROJECT_ID`. Using the ID is one of
    the most common sources of `INVALID_ARGUMENT` errors when minting tokens. The
    `gcloud projects describe ... --format='value(projectNumber)'` call above resolves it
    for you.

---

## 1. Create a Workload Identity Pool

A pool is a container for external identities. Create one per cluster (or share one across
clusters using distinct providers):

```bash
gcloud iam workload-identity-pools create "$POOL" \
  --project="$PROJECT_ID" \
  --location="global" \
  --display-name="cwii self-hosted clusters"
```

---

## 2. Create the OIDC provider

The provider is what actually trusts your kube-apiserver. The three critical fields are:

| Flag | Value | Why |
| --- | --- | --- |
| `--issuer-uri` | Exactly your kube-apiserver `--service-account-issuer` | Must match the `iss` claim in projected tokens, and Google fetches `<issuer>/.well-known/openid-configuration` from it. |
| `--allowed-audiences` | The value you will set in `cwii.dev/gcp-audience` | Must match the `aud` claim. Mismatch here is the **#1** cause of failures. |
| `--attribute-mapping` | `google.subject=assertion.sub` | Maps the token's `sub` claim onto Google's `google.subject`. |

```bash
gcloud iam workload-identity-pools providers create-oidc "$PROVIDER" \
  --project="$PROJECT_ID" \
  --location="global" \
  --workload-identity-pool="$POOL" \
  --issuer-uri="$ISSUER_URI" \
  --allowed-audiences="https://cwii.dev/gcp" \
  --attribute-mapping="google.subject=assertion.sub"
```

cwii projects standard OIDC JWTs whose `sub` claim is
`system:serviceaccount:<namespace>:<serviceaccount>`. With the mapping above,
`google.subject` becomes exactly that string — which is what you reference in IAM bindings
in [Step 4](#4-grant-access).

!!! tip "Picking the audience value"
    The `--allowed-audiences` value is **your choice** — any stable string works as long as
    it matches what cwii projects. `https://cwii.dev/gcp` is a reasonable convention. The
    cwii server has a `--gcp-default-audience` flag (and Helm value) that applies when a pod
    does not set `cwii.dev/gcp-audience`; if you set a cluster-wide default, point
    `--allowed-audiences` at that instead. Whatever you choose, the provider's
    `--allowed-audiences`, the projected-token audience, and the `audience` field inside the
    generated `credentials.json` must all be identical.

!!! warning "Attribute conditions and mapping typos"
    If you add an `--attribute-condition` (e.g. to restrict which namespaces may federate),
    a typo or a reference to an unmapped attribute silently denies **all** tokens. Keep
    conditions minimal until the happy path works, then tighten. A condition restricting to
    one namespace looks like:

    ```text
    assertion.sub.startsWith("system:serviceaccount:prod:")
    ```

---

## 3. Derive the `cwii.dev/gcp-audience` string

The **audience** that cwii must use is the **full provider resource name**, prefixed with
`//iam.googleapis.com/`. Read it back from the provider you just created:

```bash
gcloud iam workload-identity-pools providers describe "$PROVIDER" \
  --project="$PROJECT_ID" \
  --location="global" \
  --workload-identity-pool="$POOL" \
  --format='value(name)'
```

That prints the canonical resource path:

```text
projects/PROJECT_NUMBER/locations/global/workloadIdentityPools/cwii-pool/providers/cwii-cluster
```

The value you annotate onto pods (`cwii.dev/gcp-audience`) is that path with the
`//iam.googleapis.com/` prefix:

```text
//iam.googleapis.com/projects/PROJECT_NUMBER/locations/global/workloadIdentityPools/cwii-pool/providers/cwii-cluster
```

Capture it for later:

```bash
export GCP_AUDIENCE="//iam.googleapis.com/projects/${PROJECT_NUMBER}/locations/global/workloadIdentityPools/${POOL}/providers/${PROVIDER}"
echo "$GCP_AUDIENCE"
```

!!! danger "The audience must match in three places"
    `--allowed-audiences` (Step 2) is the audience your **token** carries
    (`https://cwii.dev/gcp` above). `$GCP_AUDIENCE` (this step) is the **provider resource
    name** that goes into `credentials.json.audience`. These are two **different** strings
    that serve two different roles — do not confuse them. The token `aud` must be in the
    provider's `--allowed-audiences`; the `credentials.json` `audience` must be the provider
    resource name. cwii fills `credentials.json.audience` from `cwii.dev/gcp-audience`, and
    projects the token with the audience from `cwii.dev/gcp-audience` as well — so set
    `cwii.dev/gcp-audience` to the **provider resource name**, and add that resource name to
    `--allowed-audiences` so the STS exchange accepts it.

---

## 4. Grant access

There are two ways to give your federated identity permission to do anything. Pick **one**
per workload.

### (a) Direct federation — no GSA

Bind IAM roles **directly** to the federated principal. No Google service account is
involved, and you **do not** set `cwii.dev/gcp-service-account`. cwii then builds a
direct-federation `credentials.json` (no impersonation URL).

Reference a single ServiceAccount with a `principal://` member, or a whole set with
`principalSet://`:

```bash
# Pool-scoped resource name without the //iam.googleapis.com/ prefix:
export WIP="projects/${PROJECT_NUMBER}/locations/global/workloadIdentityPools/${POOL}"

# A single Kubernetes ServiceAccount: namespace "prod", SA "checkout"
gcloud storage buckets add-iam-policy-binding "gs://my-bucket" \
  --role="roles/storage.objectViewer" \
  --member="principal://iam.googleapis.com/${WIP}/subject/system:serviceaccount:prod:checkout"
```

```bash
# Or every identity in the pool (use sparingly; prefer attribute conditions to scope it):
gcloud storage buckets add-iam-policy-binding "gs://my-bucket" \
  --role="roles/storage.objectViewer" \
  --member="principalSet://iam.googleapis.com/${WIP}/*"
```

!!! note
    The `subject/...` value in a `principal://` member is exactly the `google.subject`
    produced by your attribute mapping — i.e. `system:serviceaccount:NS:SA`.

### (b) Impersonation — federate, then impersonate a GSA

Here the federated identity is granted permission to **impersonate** an existing Google
service account (GSA), and the GSA holds the actual resource roles. Use this when you want
to reuse GSAs that other systems already trust, or when an API only accepts a GSA.

```bash
export GSA="cwii-checkout@${PROJECT_ID}.iam.gserviceaccount.com"

# 1. Let the federated principal impersonate the GSA.
gcloud iam service-accounts add-iam-policy-binding "$GSA" \
  --project="$PROJECT_ID" \
  --role="roles/iam.workloadIdentityUser" \
  --member="principal://iam.googleapis.com/${WIP}/subject/system:serviceaccount:prod:checkout"

# 2. Give the GSA the resource roles it actually needs.
gcloud storage buckets add-iam-policy-binding "gs://my-bucket" \
  --role="roles/storage.objectViewer" \
  --member="serviceAccount:${GSA}"
```

Then set `cwii.dev/gcp-service-account=$GSA` on the workload (Step 5). cwii detects the
annotation and adds a `service_account_impersonation_url` to `credentials.json`.

!!! warning "Impersonation needs **both** bindings"
    Granting `roles/iam.workloadIdentityUser` only lets the principal *become* the GSA — it
    grants **no** resource access by itself. The GSA must **also** hold the roles on the
    target resource (step 2 above). Forgetting the second binding produces a successful
    token exchange followed by `403 PERMISSION_DENIED` on the actual API call.

---

## 5. Annotate your workload

Add the cwii annotations to the **pod template** (not just the Deployment metadata, though
cwii's owner walk will read Deployment-level annotations too — see
[Annotations reference](./annotations.md) for precedence rules).

### Direct federation example

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: checkout
  namespace: prod
spec:
  replicas: 1
  selector:
    matchLabels:
      app: checkout
  template:
    metadata:
      labels:
        app: checkout
      annotations:
        cwii.dev/gcp-inject: "true"
        cwii.dev/gcp-audience: "//iam.googleapis.com/projects/123456789012/locations/global/workloadIdentityPools/cwii-pool/providers/cwii-cluster"
    spec:
      serviceAccountName: checkout
      containers:
        - name: app
          image: ghcr.io/example/checkout:1.0.0
```

### Impersonation example

Identical, plus the GSA annotation:

```yaml
      annotations:
        cwii.dev/gcp-inject: "true"
        cwii.dev/gcp-audience: "//iam.googleapis.com/projects/123456789012/locations/global/workloadIdentityPools/cwii-pool/providers/cwii-cluster"
        cwii.dev/gcp-service-account: "cwii-checkout@my-project.iam.gserviceaccount.com"
```

### What cwii injects

When the mutating webhook (`mutate.cwii.dev`) admits the pod, it makes the following
changes. After mutation it stamps the pod with `cwii.dev/injected` — a comma-joined, sorted
list of provider abbreviations (e.g. `gcp`, or `aws,gcp` if multiple providers are enabled).

**1. A projected ServiceAccount token volume.** Each enabled provider gets its **own**
projected `serviceAccountToken` volume — this is the core of cwii's multi-cloud design,
because each cloud requires a different token audience. For GCP:

| Property | Value |
| --- | --- |
| Volume name | `cwii-gcp-token` |
| Mount path (read-only) | `/var/run/secrets/cwii.dev/gcp` |
| Token file | `token` → `/var/run/secrets/cwii.dev/gcp/token` |
| Audience | `cwii.dev/gcp-audience` |
| `expirationSeconds` | `cwii.dev/gcp-token-expiration` (default `3600`, Kubernetes min `600`) |

**2. A `credentials.json` Google `external_account` credential.** Direct federation:

```json
{
  "type": "external_account",
  "audience": "//iam.googleapis.com/projects/123456789012/locations/global/workloadIdentityPools/cwii-pool/providers/cwii-cluster",
  "subject_token_type": "urn:ietf:params:oauth:token-type:jwt",
  "token_url": "https://sts.googleapis.com/v1/token",
  "token_info_url": "https://sts.googleapis.com/v1/introspect",
  "credential_source": {
    "file": "/var/run/secrets/cwii.dev/gcp/token"
  }
}
```

With `cwii.dev/gcp-service-account` set, cwii adds the impersonation URL (everything else is
identical):

```json
{
  "type": "external_account",
  "audience": "//iam.googleapis.com/projects/123456789012/locations/global/workloadIdentityPools/cwii-pool/providers/cwii-cluster",
  "subject_token_type": "urn:ietf:params:oauth:token-type:jwt",
  "token_url": "https://sts.googleapis.com/v1/token",
  "token_info_url": "https://sts.googleapis.com/v1/introspect",
  "service_account_impersonation_url": "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/cwii-checkout@my-project.iam.gserviceaccount.com:generateAccessToken",
  "credential_source": {
    "file": "/var/run/secrets/cwii.dev/gcp/token"
  }
}
```

**3. The `GOOGLE_APPLICATION_CREDENTIALS` env var**, pointing at the mounted credential:

```text
GOOGLE_APPLICATION_CREDENTIALS=/var/run/secrets/cwii.dev/gcp-creds/credentials.json
```

The Google Cloud client libraries and `gcloud` discover this path automatically — your
application code needs **no** changes.

### Credential delivery: ConfigMap vs init container

The `credentials.json` file has to reach the `cwii-gcp-creds` volume somehow. cwii supports
two delivery modes, selected with `cwii.dev/gcp-delivery` (or the server's `--gcp-delivery`
flag / Helm value). Both result in the file being mounted at
`/var/run/secrets/cwii.dev/gcp-creds/credentials.json`.

| Mode | `cwii.dev/gcp-delivery` | How it works | Cluster writes? |
| --- | --- | --- | --- |
| ConfigMap | `config-map` | The webhook **server-side-applies** a ConfigMap named `cwii-gcp-creds-<6 hex of sha256(audience+NUL+sa)>`, labeled `app.kubernetes.io/managed-by=cwii`, and mounts it via a `configMap` volume `cwii-gcp-creds`. | **Yes** — requires ConfigMap-write RBAC. |
| Init container | `init-container` | An `emptyDir` volume `cwii-gcp-creds` plus an init container `cwii-gcp-creds-writer` (image `busybox:stable`) writes `credentials.json` from the env var `CWII_GCP_CREDS_JSON`. | **No** cluster writes. |

!!! note "RBAC implication"
    cwii follows least privilege: its ClusterRole only gains
    `configmaps: get/create/update/patch` **when ConfigMap delivery is enabled**. If you run
    exclusively with `init-container` delivery, cwii never writes to the cluster. See the
    [Install](./install.md) guide for the full RBAC matrix.

---

## 6. Verify the wiring

Set `cwii.dev/gcp-verify: "true"` to have cwii add a **non-blocking** `can-i` init container
named `cwii-gcp-verify` (init order `10`, after the credential writer at order `0`). It runs:

```bash
gcloud auth application-default print-access-token
```

using the `google/cloud-sdk:slim` image. By default the check is wrapped so it **always
exits 0** and only logs failures (`<check> || echo ... >&2`), so a misconfiguration shows up
in the init-container logs without crash-looping your pod:

```bash
kubectl logs -n prod deploy/checkout -c cwii-gcp-verify
```

To make a failed check **block** pod startup (the check runs bare, so a non-zero exit fails
the init container), add:

```yaml
        cwii.dev/gcp-verify: "true"
        cwii.dev/gcp-verify-enforce: "true"
```

You can override the verify image per-pod with `cwii.dev/gcp-verify-image`, or cluster-wide
with the Helm value `providers.gcp.verifyImage`.

For the full verification workflow — including reading logs, interpreting common STS errors,
and the equivalent flow for AWS and Azure — see [Verification](./verification.md).

---

## Gotchas checklist

!!! danger "The usual suspects"
    - **Project *number*, not project *ID*.** Pool resource names use the numeric
      `projectNumber`. (Step 0.)
    - **Audience mismatch is the #1 error.** The token `aud` must appear in the provider's
      `--allowed-audiences`, and `credentials.json.audience` must be the provider resource
      name. Triple-check both with `gcloud iam workload-identity-pools providers describe`.
    - **`--issuer-uri` must equal `--service-account-issuer`** byte-for-byte, and Google must
      be able to fetch `<issuer>/.well-known/openid-configuration` over public HTTPS.
      (See [Self-hosted OIDC setup](./self-hosted-oidc.md).)
    - **Attribute-condition / attribute-mapping typos** silently deny every token. Start
      without a condition, confirm the happy path, then tighten.
    - **Impersonation needs *both* bindings.** `roles/iam.workloadIdentityUser` on the GSA
      *and* the GSA holding resource roles on the target. The first without the second yields
      `403 PERMISSION_DENIED` after a successful token exchange.
    - **Token propagation lag.** Newly published JWKS or rotated signing keys can take a few
      minutes to be fetched by Google's STS — give it time before assuming a misconfig.

## See also

- [Self-hosted OIDC setup](./self-hosted-oidc.md) — the hard prerequisite.
- [Install](./install.md) — deploying cwii and its RBAC.
- [Annotations reference](./annotations.md) — every `cwii.dev/*` annotation and precedence.
- [Verification](./verification.md) — the `can-i` init containers in depth.
