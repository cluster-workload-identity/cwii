# Installation

This guide walks platform engineers through installing **cwii** (Cluster Workload Identity Injector) on a self-hosted Kubernetes cluster. cwii is a Rust mutating admission webhook that lets your pods authenticate to **GCP**, **AWS**, and **Azure** using their Kubernetes ServiceAccount tokens — workload identity federation with **no static keys**.

> [!NOTE]
> cwii mutates pods at admission time based on annotations. Once installed, you opt workloads in with `cwii.dev/<provider>-inject` annotations. See [./annotations.md](./annotations.md) for the full annotation reference and [./verification.md](./verification.md) for the optional `can-i` verify init containers.

---

## Prerequisites

| Requirement | Detail |
| --- | --- |
| **Kubernetes** | `>= 1.24` |
| **TLS for the webhook** | [cert-manager](https://cert-manager.io/) is recommended; a Helm `genSignedCert` self-signed fallback is available if you cannot run cert-manager. |
| **Cluster OIDC** | Your `kube-apiserver` **must** publish a public HTTPS OIDC discovery document and JWKS that the cloud STS endpoints can reach. This is the hard prerequisite — see [./self-hosted-oidc.md](./self-hosted-oidc.md). |
| **Helm** | `>= 3.8` (for OCI registry support) |

> [!IMPORTANT]
> Workload identity federation only works if the cloud provider's STS can fetch your cluster's `/.well-known/openid-configuration` and JWKS over the public internet. If you have not set up `--service-account-issuer`, `--service-account-jwks-uri`, and the related signing flags on your API server, **stop here** and complete [./self-hosted-oidc.md](./self-hosted-oidc.md) first. cwii cannot inject working credentials without it.

---

## Quick install (Helm from OCI)

cwii ships as an OCI Helm chart at `oci://ghcr.io/cluster-workload-identity/charts/cwii` and a distroless image at `ghcr.io/cluster-workload-identity/cwii`. The conventional install namespace is `cwii-system`.

```bash
helm install cwii oci://ghcr.io/cluster-workload-identity/charts/cwii \
  -n cwii-system \
  --create-namespace
```

To pin a chart version:

```bash
helm install cwii oci://ghcr.io/cluster-workload-identity/charts/cwii \
  --version 1.0.0 \
  -n cwii-system \
  --create-namespace
```

### Installing from a local chart checkout

If you have cloned [github.com/cluster-workload-identity/cwii](https://github.com/cluster-workload-identity/cwii):

```bash
helm install cwii ./charts/cwii \
  -n cwii-system \
  --create-namespace
```

### Verify the deployment

```bash
kubectl -n cwii-system rollout status deploy/cwii
kubectl get mutatingwebhookconfiguration -l app.kubernetes.io/name=cwii
helm test cwii -n cwii-system   # runs the /healthz smoke test
```

---

## Key Helm values

The table below summarizes the most commonly tuned values from `values.yaml`. Override them with `--set key=value` or a `-f my-values.yaml` file.

| Value | Description | Default / notes |
| --- | --- | --- |
| `image.repository` | Webhook image | `ghcr.io/cluster-workload-identity/cwii` |
| `image.tag` | Image tag | Chart `appVersion` |
| `image.pullPolicy` | Image pull policy | `IfNotPresent` |
| `replicaCount` | Webhook replicas | Set `>= 2` when `failurePolicy=Fail` |
| `logLevel` | Server log verbosity | e.g. `info` |
| `mountRoot` | Root for injected projected-token mounts (`--mount-root`) | `/var/run/secrets/cwii.dev` |
| `tokenExpirationSeconds` | Default projected-token lifetime (`--token-expiration`) | `3600` (Kubernetes minimum `600`) |
| `providers.gcp.enabled` | Enable the GCP path (`--gcp-enabled`) | `true` |
| `providers.gcp.defaultAudience` | Default GCP token audience (`--gcp-default-audience`) | — |
| `providers.gcp.deliveryMode` | `configMap` or `initContainer` (`--gcp-delivery`) | — |
| `providers.gcp.initImage` | GCP creds-writer init image (`--gcp-init-image`) | `busybox:stable` |
| `providers.gcp.verifyImage` | GCP verify init image (`--gcp-verify-image`) | `google/cloud-sdk:slim` |
| `providers.aws.enabled` | Enable the AWS path (`--aws-enabled`) | `true` |
| `providers.aws.defaultAudience` | Default AWS token audience (`--aws-default-audience`) | `sts.amazonaws.com` |
| `providers.aws.verifyImage` | AWS verify init image (`--aws-verify-image`) | `amazon/aws-cli:latest` |
| `providers.az.enabled` | Enable the Azure path (`--az-enabled`) | `true` |
| `providers.az.defaultAudience` | Default Azure token audience (`--az-default-audience`) | `api://AzureADTokenExchange` |
| `providers.az.verifyImage` | Azure verify init image (`--az-verify-image`) | `mcr.microsoft.com/azure-cli:latest` |
| `webhook.failurePolicy` | `Ignore` (default) or `Fail` | See [failurePolicy](#failurepolicy-required-webhook--deadlock-safety) |
| `webhook.reinvocationPolicy` | Webhook reinvocation policy | `Never` |
| `webhook.timeoutSeconds` | Admission timeout | — |
| `webhook.sideEffects` | Side-effect class | `NoneOnDryRun` |
| `webhook.namespaceSelector.excludeNamespaces` | Extra namespaces to exclude | Release ns + `kube-system` + `kube-node-lease` are **always** excluded |
| `webhook.namespaceSelector.matchLabels` | Restrict to namespaces with these labels | — |
| `webhook.objectSelector` | Restrict to pods matching this selector | — |
| `webhook.matchConditions` | CEL match conditions | — |
| `tls.certManager.enabled` | Use cert-manager for TLS | `true` |
| `tls.selfSigned.durationDays` | Self-signed cert validity | Used when `tls.certManager.enabled=false` |
| `podDisruptionBudget.enabled` | Create a PDB | Recommended with `failurePolicy=Fail` |
| `resources` | CPU/memory requests & limits | — |
| `priorityClassName` | Pod priority class | — |

> [!TIP]
> The defaults shown for audiences, expiration, mount root, and images come from the server's clap flags. Per-pod overrides are available through `cwii.dev/*` annotations (see [./annotations.md](./annotations.md)) and always take precedence over chart-level defaults.

---

## TLS

The webhook serves HTTPS on `0.0.0.0:8443` and reads its certificate from `/tls/tls.crt` and key from `/tls/tls.key` (server flags `--tls-cert` / `--tls-key`).

### cert-manager (default)

With `tls.certManager.enabled=true` (the default), the chart creates a cert-manager `Certificate` and relies on the `cert-manager.io/inject-ca-from` annotation to populate the webhook's `caBundle`. The chart **does not** set `caBundle` itself — cert-manager's CA injector does.

```yaml
# values.yaml
tls:
  certManager:
    enabled: true
```

### Self-signed fallback

If you cannot run cert-manager, set `tls.certManager.enabled=false`. The chart then uses Helm's `genSignedCert` to template the TLS `Secret` and the webhook `caBundle` together in one render.

```yaml
# values.yaml
tls:
  certManager:
    enabled: false
  selfSigned:
    durationDays: 365
```

> [!WARNING]
> The self-signed fallback **does not auto-rotate**. The certificate is regenerated only when you run `helm template` / `helm upgrade`. Schedule periodic upgrades (or a manual rotation) before `selfSigned.durationDays` elapses, and prefer cert-manager in production.

---

## failurePolicy: "required webhook" & deadlock safety

The `MutatingWebhookConfiguration` is named **`mutate.cwii.dev`**, matches **pods on `CREATE`**, and uses `sideEffects=NoneOnDryRun` with `reinvocationPolicy=Never`.

By default `failurePolicy` is **`Ignore`**: if the webhook is unreachable, pods are admitted **without** injection. Setting `failurePolicy=Fail` makes injection effectively *required* — but a required webhook that cannot answer will **block all pod creation** in its scope, including its own replacement pods. That is a cluster-wide deadlock risk.

cwii ships with deadlock safety built in: the chart's `namespaceSelector` **always excludes** the release namespace, `kube-system`, and `kube-node-lease`, so a wedged webhook can never block its own pods or critical system controllers.

```yaml
# values.yaml — a safer "required" posture
replicaCount: 2

webhook:
  failurePolicy: Fail
  namespaceSelector:
    matchLabels:
      cwii.dev/inject: "enabled"   # opt-in: only these namespaces are in scope

podDisruptionBudget:
  enabled: true
```

> [!CAUTION]
> If you set `failurePolicy: Fail`, pair it with **all** of the following:
>
> - `replicaCount >= 2` so a single pod restart never makes the webhook unavailable,
> - a `PodDisruptionBudget` so voluntary disruptions keep at least one replica serving,
> - an **opt-in** `objectSelector` or `namespaceSelector.matchLabels` so the required webhook only governs the workloads you intend — not the whole cluster.
>
> The built-in exclusions (release ns, `kube-system`, `kube-node-lease`) remain in effect regardless of your selectors.

---

## RBAC

The chart's `ClusterRole` grants only what the webhook needs to resolve annotation precedence by walking pod owners:

| API group | Resources | Verbs |
| --- | --- | --- |
| core | `namespaces`, `serviceaccounts` | `get`, `list`, `watch` |
| `apps` | `deployments`, `statefulsets`, `daemonsets`, `replicasets` | `get`, `list`, `watch` |
| `batch` | `jobs` | `get`, `list`, `watch` |
| core | `configmaps` | `get`, `create`, `update`, `patch` — **only when GCP `configMap` delivery is enabled** |

> [!NOTE]
> ConfigMap write permission is least-privilege: it is granted **only** when GCP credential delivery uses `configMap` mode (the webhook server-side-applies a managed ConfigMap). In GCP `initContainer` mode — or when GCP is disabled — cwii performs no cluster writes and ConfigMap RBAC is omitted. See [./annotations.md](./annotations.md) for `cwii.dev/gcp-delivery`.

---

## Upgrade

```bash
helm upgrade cwii oci://ghcr.io/cluster-workload-identity/charts/cwii \
  -n cwii-system
```

> [!NOTE]
> Upgrades re-render the webhook configuration and, when using the self-signed fallback, regenerate the TLS certificate. Already-running injected pods are **not** re-mutated — admission webhooks only fire on pod `CREATE`. New behavior applies to pods created after the upgrade.

---

## Uninstall

```bash
helm uninstall cwii -n cwii-system
```

After uninstalling, clean up any ConfigMaps cwii created in GCP `configMap` delivery mode (they carry the `app.kubernetes.io/managed-by=cwii` label):

```bash
kubectl delete cm -A -l app.kubernetes.io/managed-by=cwii
```

> [!IMPORTANT]
> Uninstalling cwii **does not** un-inject running pods. Pods that were already mutated keep their projected-token volumes, mounts, env vars, and (in `initContainer` mode) their injected init containers until they are recreated. Roll affected workloads if you need to remove the injected configuration.

---

## Smoke test

The chart includes a Helm test that exercises the server's `GET /healthz` endpoint:

```bash
helm test cwii -n cwii-system
```

The server image is distroless, runs as nonroot (uid `65532`), and is published multi-arch (`amd64` + `arm64`).

---

## Next steps

- [Self-hosted OIDC setup](./self-hosted-oidc.md) — the cluster prerequisite (API server issuer, JWKS, audiences).
- [Annotations reference](./annotations.md) — opt workloads in per provider and tune audiences, expirations, and delivery.
- [Verification](./verification.md) — optional `can-i` init containers to confirm federation works end-to-end.
