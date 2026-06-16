# Verification (can-i init containers)

cwii ([Cluster Workload Identity Injector](https://cwii.dev)) wires up the
plumbing your pods need to federate into GCP, AWS and Azure using their
Kubernetes ServiceAccount tokens. It mounts the projected tokens, writes the
GCP credentials file, and injects the cloud SDK environment variables. What it
**cannot** do at admission time is prove that the *cloud side* of the trust
relationship actually exists.

This page explains the optional `can-i`-style verify init containers: what they
are, when to enable them, what they run per provider, and how to read their
output.

## Why verification exists

The mutating webhook runs at pod **admission**. At that moment it has no way to
reach the cloud STS endpoints, and it has no knowledge of your IAM
configuration. So admission can succeed — the pod template looks perfectly
correct — while the *runtime* federation will fail for reasons entirely outside
the cluster, for example:

- The cloud-side workload-identity / OIDC provider was never created, or points
  at the wrong issuer URL or JWKS.
- The IAM role / GSA / app registration exists but the trust policy or
  federated-credential subject does not match `system:serviceaccount:NS:SA`.
- The projected token audience does not match what the cloud provider expects.
- Your self-hosted [`--service-account-issuer`](./self-hosted-oidc.md) is not
  publicly reachable by the cloud STS.

> [!NOTE]
> A successful pod injection only means cwii produced valid Kubernetes config.
> It is **not** a guarantee that `AssumeRoleWithWebIdentity`, GCP STS token
> exchange, or Entra ID federation will succeed at runtime. Verification closes
> that gap by actually exchanging a token before your workload starts.

Verification turns a silent runtime failure (an app that boots and then 403s on
its first cloud API call, often minutes later) into a loud, early signal you can
see in `kubectl logs` — or, with enforcement, a pod that refuses to leave `Init`.

## Enabling verification

Verification is **opt-in, per provider**, via annotations. See
[annotations.md](./annotations.md) for the full annotation reference and
[precedence rules](./annotations.md) (pod > owning workload > ServiceAccount >
namespace, evaluated independently per key).

| Annotation | Values | Effect |
| --- | --- | --- |
| `cwii.dev/<p>-verify` | `"true"` / `"false"` | Adds a non-blocking verify init container for provider `<p>`. |
| `cwii.dev/<p>-verify-enforce` | `"true"` / `"false"` | Makes a failed verify **block** pod startup (pod stays in `Init`). |
| `cwii.dev/<p>-verify-image` | image ref | Overrides the verify init-container image for provider `<p>`. |

`<p>` is one of the provider abbreviations: `gcp`, `aws`, `az`.

### Non-blocking (default)

Setting `cwii.dev/<p>-verify: "true"` alone is **non-blocking**. The check is
wrapped so that it always exits `0`:

```text
<check> || echo ... >&2
```

If the check fails, the failure is logged to stderr but the init container still
succeeds, so the pod continues to start normally. This is the recommended mode
for getting visibility without risking availability.

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: data-loader
spec:
  template:
    metadata:
      annotations:
        cwii.dev/gcp-inject: "true"
        cwii.dev/gcp-service-account: data-loader@my-project.iam.gserviceaccount.com
        # Log a warning if federation is broken, but still start the pod.
        cwii.dev/gcp-verify: "true"
    spec:
      serviceAccountName: data-loader
      containers:
        - name: app
          image: ghcr.io/example/data-loader:1.0.0
```

### Enforcing (block on failure)

Add `cwii.dev/<p>-verify-enforce: "true"` to run the check **bare** (without the
`|| echo` wrapper). A non-zero exit then fails the init container, and Kubernetes
keeps the pod in `Init` and restarts the init container per the pod's restart
policy. Use this when you would rather fail closed than run a workload that
cannot authenticate.

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: billing-exporter
spec:
  template:
    metadata:
      annotations:
        cwii.dev/aws-inject: "true"
        cwii.dev/aws-role-arn: arn:aws:iam::123456789012:role/billing-exporter
        cwii.dev/aws-region: us-east-1
        # Refuse to start unless AssumeRoleWithWebIdentity actually works.
        cwii.dev/aws-verify: "true"
        cwii.dev/aws-verify-enforce: "true"
    spec:
      serviceAccountName: billing-exporter
      containers:
        - name: app
          image: ghcr.io/example/billing-exporter:2.3.1
```

> [!WARNING]
> `cwii.dev/<p>-verify-enforce: "true"` is meaningful only together with
> `cwii.dev/<p>-verify: "true"`. Enforcement controls *how* the verify container
> runs; it does not add one on its own.

<!-- -->

> [!CAUTION]
> An enforced verify makes your pod's startup depend on a live cloud STS call.
> If the cloud STS endpoint is throttling or unreachable, enforced pods will be
> stuck in `Init` even when nothing is wrong with your IAM. Weigh this against
> the cost of starting a workload that cannot authenticate.

## What each provider's verify runs

Every verify container consumes the same projected token cwii already mounts for
that provider (read-only at `/var/run/secrets/cwii.dev/<p>/token`) and the same
injected environment / credentials file. It then performs the smallest possible
real token exchange for that cloud.

| Provider | Init container name | Command | Default image |
| --- | --- | --- | --- |
| `gcp` | `cwii-gcp-verify` | `gcloud auth application-default print-access-token` | `google/cloud-sdk:slim` |
| `aws` | `cwii-aws-verify` | `aws sts get-caller-identity` | `amazon/aws-cli:latest` |
| `az` | `cwii-az-verify` | `az login --service-principal ... --federated-token ... && az account show` | `mcr.microsoft.com/azure-cli:latest` |

- **GCP** — `gcloud auth application-default print-access-token` reads the
  injected `GOOGLE_APPLICATION_CREDENTIALS` (the `external_account`
  `credentials.json` cwii produced) and forces a token exchange against GCP STS,
  performing impersonation if `cwii.dev/gcp-service-account` is set or direct
  federation otherwise.
- **AWS** — `aws sts get-caller-identity` exercises
  `AssumeRoleWithWebIdentity` using the injected `AWS_ROLE_ARN` and
  `AWS_WEB_IDENTITY_TOKEN_FILE`, then prints the resolved caller identity.
- **Azure** — `az login --service-principal --federated-token ...` performs an
  Entra ID federated-identity-credential exchange using the injected
  `AZURE_CLIENT_ID` / `AZURE_TENANT_ID` and `AZURE_FEDERATED_TOKEN_FILE`, then
  `az account show` confirms an authenticated context.

### Overriding the verify image

The cloud-SDK images above are convenient but large and pulled from public
registries. You can pin or mirror them two ways:

- **Per workload** — `cwii.dev/<p>-verify-image: <image-ref>` on the pod, owning
  workload, ServiceAccount or namespace.
- **Cluster-wide default** — the Helm value `providers.<p>.verifyImage` (see
  [install.md](./install.md)).

> [!TIP]
> In air-gapped or egress-restricted clusters, mirror
> `google/cloud-sdk:slim`, `amazon/aws-cli:latest` and
> `mcr.microsoft.com/azure-cli:latest` into your private registry and set
> `providers.<p>.verifyImage` (or `cwii.dev/<p>-verify-image`) to the mirrored
> references. Pinning a digest also avoids the surprise of `:latest` drifting
> for the AWS and Azure images.

## Ordering

Init containers cwii injects carry an explicit ordering. Verify containers run at
**order 10**, which places them **after the GCP credentials writer** (order 0)
so that, for GCP `initContainer` delivery, `credentials.json` already exists on
disk before `cwii-gcp-verify` tries to use it.

| Order | Init container | Purpose |
| --- | --- | --- |
| 0 | `cwii-gcp-creds-writer` | Writes `credentials.json` (GCP `initContainer` delivery only). See [annotations.md](./annotations.md). |
| 10 | `cwii-<p>-verify` | Performs the token-exchange check for provider `<p>`. |

When verify is enabled for multiple providers, each provider's `cwii-<p>-verify`
container is independent — one provider's result never affects another.

## Reading verify output

Inspect a verify container's logs with `kubectl logs`, naming the container with
`-c`:

```bash
# GCP verify logs for a pod
kubectl logs <pod> -c cwii-gcp-verify

# AWS verify logs
kubectl logs <pod> -c cwii-aws-verify

# Azure verify logs
kubectl logs <pod> -c cwii-az-verify
```

For an enforced verify that failed, the pod will be in `Init:Error` /
`Init:CrashLoopBackOff`; use `--previous` to read the last failed attempt:

```bash
kubectl logs <pod> -c cwii-aws-verify --previous
kubectl describe pod <pod>   # shows the init container exit code and reason
```

### Example: success

A healthy AWS verify (`aws sts get-caller-identity`) prints the resolved
identity and exits `0`:

```text
{
    "UserId": "AROAEXAMPLEID:botocore-session-1700000000",
    "Account": "123456789012",
    "Arn": "arn:aws:sts::123456789012:assumed-role/billing-exporter/botocore-session-1700000000"
}
```

The `assumed-role` ARN confirms `AssumeRoleWithWebIdentity` succeeded against the
role in `cwii.dev/aws-role-arn`.

### Example: failure (non-blocking)

With `cwii.dev/aws-verify: "true"` but **no** enforce, a broken trust policy is
logged to stderr and the container still exits `0` (so the pod starts anyway):

```text
An error occurred (AccessDenied) when calling the AssumeRoleWithWebIdentity operation:
Not authorized to perform sts:AssumeRoleWithWebIdentity
cwii: verification check failed (non-enforcing)
```

The trailing `cwii: verification check failed (non-enforcing)` line is the `|| echo`
fallback that keeps the init container green.

### Example: failure (enforcing)

The identical underlying error under `cwii.dev/aws-verify-enforce: "true"` runs
bare, so the non-zero exit code propagates and the pod stays in `Init`:

```text
An error occurred (AccessDenied) when calling the AssumeRoleWithWebIdentity operation:
Not authorized to perform sts:AssumeRoleWithWebIdentity
```

```bash
$ kubectl get pod billing-exporter-xxxx
NAME                     READY   STATUS                  RESTARTS   AGE
billing-exporter-xxxx    0/1     Init:CrashLoopBackOff   3          2m
```

### Reading common failures

| Symptom in logs | Likely cause |
| --- | --- |
| GCP: `unable to generate access token`, `invalid_grant`, `Invalid value for "audience"` | `cwii.dev/gcp-audience` does not match the GCP workload-identity-pool provider audience, or the OIDC issuer is unreachable. |
| GCP: `Permission 'iam.serviceAccounts.getAccessToken' denied` | `cwii.dev/gcp-service-account` impersonation not granted (missing `roles/iam.workloadIdentityUser`). |
| AWS: `AccessDenied ... sts:AssumeRoleWithWebIdentity` | Role trust policy `sub`/`aud` condition does not match `system:serviceaccount:NS:SA` or the token audience (default `sts.amazonaws.com`). |
| AWS: `InvalidIdentityToken ... could not be retrieved from the OIDC provider` | The cluster issuer is not registered as an IAM OIDC provider, or its JWKS is not reachable. See [self-hosted-oidc.md](./self-hosted-oidc.md). |
| Azure: `AADSTS70021 No matching federated identity record found` | The Entra app's federated credential subject/issuer/audience (default `api://AzureADTokenExchange`) does not match the projected token. |
| Azure: `AADSTS700016 Application not found` | `cwii.dev/az-client-id` / `cwii.dev/az-tenant-id` are wrong. |

## What verify does NOT test

> [!IMPORTANT]
> Verification proves **identity federation only** — that the pod's
> ServiceAccount token can be exchanged for a cloud credential. It does **not**
> test resource-level IAM.

Concretely, a passing verify confirms you can obtain a token / assume the role /
log in. It does **not** confirm that the resulting identity is allowed to read
your bucket, write to your queue, call your KMS key, or perform any other
specific action. Those resource permissions are still your responsibility and
will only surface at the point your application calls them. Treat verify as a
"can I authenticate?" check, not an "am I authorized for everything?" check.

## Caveats and costs

- **Startup latency.** Each verify init container pulls a cloud-SDK image (which
  can be large) and performs a real network round-trip to a cloud STS endpoint
  before your app container starts. Mirror and pin the images
  (`providers.<p>.verifyImage`) to keep pulls fast and predictable.
- **A cloud dependency at pod start.** Verification introduces a hard dependency
  on cloud STS availability at the moment of scheduling. This is benign in
  non-enforcing mode (it only logs) but, in enforcing mode, a cloud STS outage
  or throttle will block otherwise-healthy pods in `Init`.
- **Per-provider, per-pod overhead.** Enabling verify on every provider adds one
  init container per enabled provider to every matched pod. Consider scoping
  verify to a canary workload, ServiceAccount, or namespace rather than enabling
  it cluster-wide.

## See also

- [annotations.md](./annotations.md) — full annotation reference and precedence.
- [self-hosted-oidc.md](./self-hosted-oidc.md) — publishing the OIDC discovery
  document and JWKS the cloud STS endpoints must fetch.
- [install.md](./install.md) — Helm values, including `providers.<p>.verifyImage`.
