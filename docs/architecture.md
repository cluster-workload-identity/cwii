# Architecture

This document explains how **cwii** (Cluster Workload Identity Injector) works under the
hood: the components it deploys, the request flow through the mutating admission webhook,
and the design decisions that make multi-cloud workload identity federation work on
self-hosted Kubernetes clusters.

cwii is a Rust Kubernetes **mutating admission webhook**. It lets pods on self-hosted
clusters authenticate to **GCP**, **AWS**, and **Azure** using their Kubernetes
ServiceAccount tokens — workload identity federation, with **no static keys**.

| | |
|---|---|
| Repository | [`github.com/cluster-workload-identity/cwii`](https://github.com/cluster-workload-identity/cwii) |
| Site | [`cwii.dev`](https://cwii.dev) |
| Image | `ghcr.io/cluster-workload-identity/cwii` |
| Helm chart | `oci://ghcr.io/cluster-workload-identity/charts/cwii` |
| Install namespace | `cwii-system` |

!!! note "The hard prerequisite"
    Every cloud STS endpoint that cwii targets must be able to fetch your cluster's
    OIDC discovery document and JWKS over HTTPS. Configuring the kube-apiserver to
    publish these is the one piece of setup cwii cannot do for you. See
    [Self-hosted OIDC setup](./self-hosted-oidc.md).

---

## High-level picture

```text
                          ┌──────────────────────────────────────────┐
   kubectl apply pod      │              kube-apiserver                │
 ────────────────────────▶                                            │
                          │   admission chain                         │
                          │   ┌────────────────────────────────────┐  │
                          │   │ MutatingWebhookConfiguration         │ │
                          │   │   mutate.cwii.dev (pods CREATE)      │ │
                          │   └───────────────┬────────────────────┘  │
                          └───────────────────┼──────────────────────┘
                                              │ AdmissionReview (Pod)
                                              ▼  HTTPS POST /mutate
                          ┌──────────────────────────────────────────┐
                          │           cwii webhook (Rust)             │
                          │  axum + rustls  •  GET /healthz           │
                          │                                           │
                          │  resolve annotations (pod>owner>sa>ns)    │
                          │  per-provider Provider.plan()             │
                          │  merge plans → RFC 6902 JSON patch        │
                          │  (upsert GCP ConfigMap unless dry-run)    │
                          └───────────────────┬──────────────────────┘
                                              │ JSONPatch
                                              ▼
                          ┌──────────────────────────────────────────┐
                          │            Mutated Pod spec               │
                          │  cwii-gcp-token  →  /…/cwii.dev/gcp/token │
                          │  cwii-aws-token  →  /…/cwii.dev/aws/token │
                          │  cwii-az-token   →  /…/cwii.dev/az/token  │
                          │  + env / creds.json / init containers     │
                          └──────────────────────────────────────────┘
                                              │  pod runs
                                              ▼
                      GCP STS  •  AWS STS  •  Entra ID  (federation)
```

---

## Components

cwii deploys and manages a small set of objects. The webhook process itself is the only
long-running component; everything else is created or patched on demand.

| Component | What it is | When it exists |
|---|---|---|
| **Webhook server** | Rust [`axum`](https://github.com/tokio-rs/axum) + [`rustls`](https://github.com/rustls/rustls) HTTPS service exposing `GET /healthz` and `POST /mutate`. | Always (the Deployment). |
| **kube client** | In-process client used to read namespaces, ServiceAccounts, and owning workloads, and to write GCP ConfigMaps. | Always. |
| **MutatingWebhookConfiguration** | `mutate.cwii.dev`, intercepts pod `CREATE`. | Always (installed by the chart). |
| **Per-namespace ConfigMaps** | `cwii-gcp-creds-<hash>` holding GCP `credentials.json`, server-side-applied into the pod's namespace. | Only in GCP **configMap** delivery mode. |
| **Init containers** | GCP creds writer, plus opt-in per-provider verify containers. | Only when the relevant feature is enabled for the pod. |
| **Per-provider projected token volumes** | `cwii-<p>-token` projected ServiceAccount token volumes, one per enabled provider. | One per provider cwii injects. |

### Rust workspace layout

The codebase is a Cargo workspace. Provider logic is isolated behind a single trait so
each cloud can evolve independently.

| Crate | Responsibility |
|---|---|
| `cwii-core` | The `Provider` trait, the plan intermediate representation (IR), annotation **resolve** logic, RFC 6902 **patch** building, and admission glue. |
| `cwii-provider-gcp` | GCP `external_account` credentials, ConfigMap/init-container delivery, GCP verify. |
| `cwii-provider-aws` | AWS env-var injection (`AssumeRoleWithWebIdentity`), AWS verify. |
| `cwii-provider-az` | Azure env-var injection (Entra ID federated identity), Azure verify. |
| `cwii` | The binary: CLI/flag parsing, HTTP server, TLS, wiring the providers together. |

!!! tip "Why a `Provider` trait"
    Each provider produces an independent **plan** (volumes, mounts, env, init
    containers, optional ConfigMap). `cwii-core` merges these plans and emits one patch.
    Adding a fourth cloud means writing a new crate that implements the trait — no
    changes to the core merge/patch path.

---

## Request flow

When the API server admits a pod, the following happens:

1. **API server → AdmissionReview.** kube-apiserver matches the pod `CREATE` against the
   `mutate.cwii.dev` webhook and POSTs an `AdmissionReview` to `https://…/mutate`.
2. **Resolve annotations.** For each annotation key, cwii walks the precedence chain
   (pod → owning workload → ServiceAccount → namespace) and takes the first explicit
   value. Each key is resolved **independently** (see [Precedence](#annotation-precedence)).
3. **Per-provider `Provider.plan()`.** For every enabled provider, cwii computes a plan:
   the projected token volume, mounts, env vars, credentials, and any init containers.
4. **Merge plans.** `cwii-core` merges the per-provider plans into a single set of
   spec changes.
5. **Upsert GCP ConfigMap** (configMap delivery only). Unless the request is a dry run,
   the webhook server-side-applies the `cwii-gcp-creds-<hash>` ConfigMap into the pod's
   namespace. On dry-run requests, no cluster writes occur.
6. **RFC 6902 JSON patch.** cwii returns the merged changes as a JSON Patch in the
   `AdmissionResponse`. The API server applies it and writes the marker annotation
   `cwii.dev/injected`.

```text
 CREATE Pod ─▶ AdmissionReview ─▶ resolve(pod>owner>sa>ns)
                                        │
                                        ▼
                     ┌──── gcp.plan() ──┐
                     ├──── aws.plan() ──┤──▶ merge ──▶ (upsert CM unless dryRun) ──▶ JSONPatch
                     └──── az.plan()  ──┘
```

---

## Annotation precedence

cwii reads annotations with the prefix `cwii.dev/`. Provider abbreviations are `gcp`,
`aws`, and `az`.

The precedence chain, **highest to lowest**, is:

```text
pod  >  owning workload  >  ServiceAccount  >  namespace
```

Key rules:

- **Evaluated independently per key.** `cwii.dev/gcp-audience` and
  `cwii.dev/aws-inject` are resolved separately; a value set high for one key does not
  carry over to another.
- **First explicit value wins.** A specific `"false"` suppresses a broader `"true"`.
  For example, `cwii.dev/aws-inject: "false"` on a pod overrides
  `cwii.dev/aws-inject: "true"` on its namespace.
- **One provider never affects another.** Enabling GCP has no bearing on whether AWS or
  Azure are injected.

The owner walk handles `ReplicaSet → Deployment` (Deployment annotations are preferred
over the intermediate ReplicaSet), plus `StatefulSet`, `DaemonSet`, and `Job`.

!!! example "A specific false suppresses a broader true"
    ```yaml
    # Namespace: enable AWS for everything by default
    apiVersion: v1
    kind: Namespace
    metadata:
      name: team-a
      annotations:
        cwii.dev/aws-inject: "true"
    ---
    # Pod: opt this one workload out — pod-level "false" wins
    apiVersion: v1
    kind: Pod
    metadata:
      name: no-aws-here
      namespace: team-a
      annotations:
        cwii.dev/aws-inject: "false"
    ```

See the full [Annotations reference](./annotations.md) for every supported key.

---

## The key design: per-provider token separation

This is the central design decision in cwii.

**Each enabled provider gets its own projected `serviceAccountToken` volume.** They are
never shared.

| Property | Value |
|---|---|
| Volume name | `cwii-<p>-token` |
| Mount path | `/var/run/secrets/cwii.dev/<p>` (read-only) |
| File name | `token` |
| Token path | `/var/run/secrets/cwii.dev/<p>/token` |
| `expirationSeconds` | default `3600`, minimum `600` |

```yaml
# What cwii adds for, e.g., AWS (illustrative):
volumes:
  - name: cwii-aws-token
    projected:
      sources:
        - serviceAccountToken:
            path: token
            audience: sts.amazonaws.com   # provider-specific audience
            expirationSeconds: 3600
# ...mounted read-only into each container at:
#   /var/run/secrets/cwii.dev/aws  ->  file "token"
```

**Why separate volumes?**

- **Different audiences per cloud.** GCP, AWS, and Azure each require the projected token
  to carry a *different* `aud` claim. The audience is set on the projected volume, so a
  pod talking to two clouds genuinely needs two tokens.
- **Isolation.** A token minted for AWS STS is never visible at the path another provider
  reads, limiting blast radius if a token leaks.
- **Works even with `automountServiceAccountToken: false`.** cwii's volumes are explicit,
  independent projected volumes. They do not rely on the default ServiceAccount token
  automount, so injection works regardless of that setting.

!!! info "Audience defaults"
    | Provider | Default audience |
    |---|---|
    | GCP | (set per workload via `cwii.dev/gcp-audience` / `--gcp-default-audience`) |
    | AWS | `sts.amazonaws.com` |
    | Azure | `api://AzureADTokenExchange` |

    Projected SA tokens are standard OIDC JWTs with `sub = system:serviceaccount:NS:SA`.
    Override the per-provider audience with `cwii.dev/<p>-audience` and the lifetime with
    `cwii.dev/<p>-token-expiration` (Kubernetes minimum 600 seconds, default 3600).

### GCP injection

GCP is the only provider that materializes a credentials file. cwii builds a Google
`external_account` `credentials.json`:

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

- If `cwii.dev/gcp-service-account` (a GSA email) is set, cwii adds
  `service_account_impersonation_url` and the pod **impersonates** that GSA:

  ```text
  https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/<GSA>:generateAccessToken
  ```

  Without it, GCP uses **direct federation** (the federated identity acts directly).

- `credentials.json` is mounted at `/var/run/secrets/cwii.dev/gcp-creds/credentials.json`,
  and the env var `GOOGLE_APPLICATION_CREDENTIALS` points there.

#### Delivery modes: `configMap` vs `initContainer`

Choose with `cwii.dev/gcp-delivery` (`config-map` | `init-container`) or the
`--gcp-delivery` flag.

=== "configMap"

    The webhook server-side-applies a ConfigMap named
    `cwii-gcp-creds-<6 hex of sha256(audience + NUL + sa)>`, labeled
    `app.kubernetes.io/managed-by=cwii`. It is mounted into the pod via a `configMap`
    volume named `cwii-gcp-creds`.

    - **Requires** ConfigMap-write RBAC (see [RBAC](#rbac)).
    - Content-addressed name means identical (audience, SA) pairs share one ConfigMap.

=== "initContainer"

    cwii adds an `emptyDir` volume named `cwii-gcp-creds` and an init container
    `cwii-gcp-creds-writer` (image `busybox:stable`) that writes `credentials.json`
    from the env var `CWII_GCP_CREDS_JSON`.

    - **No cluster writes** — nothing is created outside the pod.

| Trade-off | `configMap` | `initContainer` |
|---|---|---|
| Cluster writes | Yes (ConfigMap upsert) | None |
| RBAC needed | ConfigMap get/create/update/patch | None beyond reads |
| Extra init container | No | Yes (`cwii-gcp-creds-writer`) |
| Dedup across pods | Yes (content-addressed) | No |

See [GCP setup](./gcp-setup.md) for end-to-end configuration.

### AWS injection

**Env-vars only — no file is written** beyond the projected token. Mechanism:
`AssumeRoleWithWebIdentity`.

| Env var | Value |
|---|---|
| `AWS_ROLE_ARN` | `<role-arn>` (from `cwii.dev/aws-role-arn`, **required to inject**) |
| `AWS_WEB_IDENTITY_TOKEN_FILE` | `/var/run/secrets/cwii.dev/aws/token` |
| `AWS_REGION` | optional (`cwii.dev/aws-region`) |
| `AWS_ROLE_SESSION_NAME` | optional (`cwii.dev/aws-role-session-name`) |

!!! warning "`cwii.dev/aws-role-arn` is required"
    AWS injection does not occur unless `cwii.dev/aws-role-arn` is set. The default token
    audience is `sts.amazonaws.com`.

See [AWS setup](./aws-setup.md).

### Azure injection

**Env-vars only.** Mechanism: Entra ID federated identity credential.

| Env var | Value |
|---|---|
| `AZURE_CLIENT_ID` | from `cwii.dev/az-client-id` (**required**) |
| `AZURE_TENANT_ID` | from `cwii.dev/az-tenant-id` (**required**) |
| `AZURE_FEDERATED_TOKEN_FILE` | `/var/run/secrets/cwii.dev/az/token` |
| `AZURE_AUTHORITY_HOST` | optional (`cwii.dev/az-authority-host`) |

!!! warning "Azure requires client and tenant IDs"
    Both `cwii.dev/az-client-id` and `cwii.dev/az-tenant-id` are required to inject. The
    default token audience is `api://AzureADTokenExchange`.

See [Azure setup](./az-setup.md).

---

## Verify ("can-i") init containers

cwii can inject an opt-in **verify** init container per provider that performs a
"can I authenticate?" check before your workload starts. Enable with
`cwii.dev/<p>-verify: "true"`.

| Provider | Container | Image | Command |
|---|---|---|---|
| GCP | `cwii-gcp-verify` | `google/cloud-sdk:slim` | `gcloud auth application-default print-access-token` |
| AWS | `cwii-aws-verify` | `amazon/aws-cli:latest` | `aws sts get-caller-identity` |
| Azure | `cwii-az-verify` | `mcr.microsoft.com/azure-cli:latest` | `az login --service-principal … --federated-token … && az account show` |

**Ordering.** Verify containers run at order **10** (after the GCP creds writer, which
runs at order **0**).

**Blocking behavior.**

- **Non-blocking by default.** The check is wrapped as `<check> || echo … >&2`, so it
  always exits `0` and only logs failures.
- **Enforcing.** With `cwii.dev/<p>-verify-enforce: "true"` the check runs **bare**, so a
  non-zero exit **blocks pod startup**.

**Image override.** Set `cwii.dev/<p>-verify-image`, or use the Helm value
`providers.<p>.verifyImage`.

See [Verification](./verification.md) for usage patterns.

---

## Idempotency and reinvocation

The webhook is safe to re-run against an already-mutated pod (it sets
`reinvocationPolicy: Never`, but defensive guards exist regardless):

- **Status marker.** cwii writes `cwii.dev/injected`, a comma-joined sorted list of the
  provider abbreviations it injected (e.g. `"aws,gcp"`).
- **Per-name guards.** Volumes, mounts, and init containers are added only if an object
  with that name (`cwii-<p>-token`, `cwii-gcp-creds`, `cwii-gcp-creds-writer`, the verify
  containers, etc.) is not already present.
- **Per-env guard.** Each injected env var is added only if not already set on the
  container.

Together these make a second mutation pass a no-op.

---

## The webhook configuration

The chart installs a `MutatingWebhookConfiguration`:

| Setting | Value |
|---|---|
| Webhook name | `mutate.cwii.dev` |
| Matches | pods, `CREATE` |
| `sideEffects` | `NoneOnDryRun` |
| `failurePolicy` | default `Ignore` (set `Fail` to **require** injection) |
| `reinvocationPolicy` | `Never` |

!!! danger "Deadlock safety"
    The `namespaceSelector` **always** excludes the release namespace, `kube-system`, and
    `kube-node-lease`. This prevents a self-deadlock — critical when
    `failurePolicy: Fail`, because otherwise the webhook could block pods needed to run
    the webhook itself.

### TLS

| Mode | How |
|---|---|
| **cert-manager** (default) | A `Certificate` plus the `cert-manager.io/inject-ca-from` annotation. The chart does **not** set `caBundle` itself — cert-manager injects it. |
| **Self-signed fallback** | Set `tls.certManager.enabled=false`. Helm `genSignedCert` templates the `Secret` and `caBundle` together. |

The server reads its cert and key from `--tls-cert` (`/tls/tls.crt`) and `--tls-key`
(`/tls/tls.key`).

---

## RBAC

cwii follows least privilege. Its `ClusterRole` grants:

| Resource | Verbs | Notes |
|---|---|---|
| `namespaces`, `serviceaccounts` | get, list, watch | for annotation resolution |
| `apps`: deployments, statefulsets, daemonsets, replicasets | get, list, watch | owner walk |
| `batch`: jobs | get, list, watch | owner walk |
| ConfigMaps | get, create, update, patch | **only** when GCP `configMap` delivery is enabled |

!!! tip
    If you never use GCP `configMap` delivery, cwii needs **no** write access to the
    cluster at all. The `initContainer` delivery mode keeps it read-only.

---

## Server flags and environment

Configuration is parsed with [`clap`](https://github.com/clap-rs/clap); every flag has a
matching env var.

| Flag | Env var | Default |
|---|---|---|
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

**Endpoints:** `GET /healthz` and `POST /mutate` (both HTTPS).

**Image:** distroless **nonroot** (uid `65532`), multi-arch `amd64` + `arm64`.

See [Install](./install.md) for chart values that map onto these flags.

---

## Security posture

| Control | Detail |
|---|---|
| Distroless nonroot | Runs as uid `65532`; no shell, no package manager in the image. |
| Read-only root filesystem | The webhook container runs with a read-only root fs. |
| Drop ALL capabilities | All Linux capabilities are dropped. |
| Short-lived tokens | Federated tokens are projected and short-lived (default 3600s, min 600s); no static cloud keys ever touch the cluster. |
| Minimal sensitive state | The only sensitive material cwii holds is its **TLS serving cert**. |

The federation model means the cloud-side credentials are minted on demand by each
cloud's STS from a short-lived OIDC token — there is nothing long-lived to leak.

---

## Failure modes

| Failure | Where it surfaces | Behavior / mitigation |
|---|---|---|
| **Webhook is down** | Admission time | Governed by `failurePolicy`. Default `Ignore` lets pods through unmutated; `Fail` blocks pod creation in matched namespaces (excluded namespaces are always safe). |
| **OIDC discovery / JWKS unreachable** | **Runtime, not admission** | The mutation still succeeds; the *cloud auth call* fails when the pod runs. This is exactly why **verify init containers** exist — they catch it early. Use `-verify-enforce` to fail the pod fast. |
| **Audience mismatch** | Runtime | The cloud STS rejects the token. Confirm the per-provider audience (`cwii.dev/<p>-audience`) matches the cloud-side trust/federation config. |
| **Clock skew** | Runtime | JWT `exp`/`nbf` validation fails at the cloud STS. Ensure nodes and the API server use NTP. |

!!! note "Admission vs runtime"
    cwii can only guarantee the pod *spec* is correct at admission time. Whether the cloud
    actually trusts the token is decided at runtime by the cloud's STS using your
    [self-hosted OIDC](./self-hosted-oidc.md) configuration. Verification containers
    bridge that gap.

---

## See also

- [Self-hosted OIDC setup](./self-hosted-oidc.md) — the kube-apiserver prerequisite.
- [Annotations reference](./annotations.md) — every `cwii.dev/*` key.
- [Verification](./verification.md) — using the can-i init containers.
- [Install](./install.md) — chart values and deployment.
- [GCP setup](./gcp-setup.md) · [AWS setup](./aws-setup.md) · [Azure setup](./az-setup.md)
