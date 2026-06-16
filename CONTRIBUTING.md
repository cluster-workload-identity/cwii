# Contributing to cwii

Thanks for your interest in **cwii** (Cluster Workload Identity Injector) — a Rust Kubernetes
mutating admission webhook that lets pods on **self-hosted** clusters authenticate to GCP, AWS and
Azure using their Kubernetes ServiceAccount tokens (workload identity federation), with **no static
keys**.

This guide covers everything you need to develop, test and extend cwii: development prerequisites,
the build/test/lint workflow, the repository layout, how the webhook works internally, how to add a
new cloud provider, project conventions, and how to exercise the mutation logic out-of-cluster.

> [!NOTE]
> If you are looking to **deploy** cwii rather than develop it, start with [`./install.md`](./install.md),
> the [annotation reference](./annotations.md), the [self-hosted OIDC prerequisites](./self-hosted-oidc.md),
> and the [verification guide](./verification.md).

---

## Table of contents

- [Project facts at a glance](#project-facts-at-a-glance)
- [Development prerequisites](#development-prerequisites)
- [Build, test and lint](#build-test-and-lint)
- [Repository layout](#repository-layout)
- [How cwii works (developer mental model)](#how-cwii-works-developer-mental-model)
- [Adding a new provider](#adding-a-new-provider)
- [Project conventions](#project-conventions)
- [Out-of-cluster testing](#out-of-cluster-testing)
- [Pull request checklist](#pull-request-checklist)

---

## Project facts at a glance

| Item | Value |
| --- | --- |
| Repository | `github.com/cluster-workload-identity/cwii` |
| Website | `cwii.dev` |
| Container image | `ghcr.io/cluster-workload-identity/cwii` |
| Helm chart | `oci://ghcr.io/cluster-workload-identity/charts/cwii` |
| Install namespace | `cwii-system` |
| Annotation prefix | `cwii.dev/` |
| Providers | `gcp`, `aws`, `az` (all fully supported) |
| Image base | distroless nonroot, uid `65532`, multi-arch `amd64` + `arm64` |

---

## Development prerequisites

| Tool | Why |
| --- | --- |
| **Rust stable** | Building and testing the workspace (`cargo build/test/clippy`). |
| **Rust nightly** | Formatting only. `rustfmt.toml` enables `unstable_features`, so `cargo fmt` must run on nightly. |
| **Helm** | Linting and templating the chart in `charts/cwii`. |
| **Docker** | Building the distroless multi-arch image and running busybox/cloud-SDK images locally. |

Install the toolchains with [`rustup`](https://rustup.rs/):

```bash
rustup toolchain install stable
rustup toolchain install nightly
rustup component add clippy --toolchain stable
rustup component add rustfmt --toolchain nightly
```

> [!TIP]
> In restricted sandboxes you may need a writable `CARGO_HOME`/`CARGO_TARGET_DIR` so the registry
> cache and build artifacts land somewhere you can write. Point them at a project-local or temp
> directory if the default `~/.cargo` is read-only:
>
> ```bash
> export CARGO_HOME="$PWD/.cargo"
> export CARGO_TARGET_DIR="$PWD/target"
> ```

---

## Build, test and lint

Run the full set of checks before opening a PR. CI runs the same commands.

```bash
# Build the entire workspace
cargo build --workspace

# Run all tests
cargo test --workspace

# Lint — warnings are errors
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Format (nightly is required because rustfmt.toml uses unstable features)
cargo +nightly fmt --all

# In CI / pre-commit, verify formatting without rewriting files
cargo +nightly fmt --all --check

# Dependency / license / advisory audit
cargo deny check

# Lint the Helm chart
helm lint charts/cwii

# Render the chart to verify templating
helm template charts/cwii
```

> [!IMPORTANT]
> `cargo fmt` **must** be invoked as `cargo +nightly fmt`. Running it on stable will fail or silently
> skip the unstable rules configured in `rustfmt.toml`.

---

## Repository layout

```text
cwii/
├── crates/
│   ├── cwii-core/          # core webhook logic: Provider trait, annotation
│   │                       #   resolution + precedence, token-volume planning,
│   │                       #   admission review mutation
│   ├── cwii-provider-gcp/  # GCP provider: external_account credentials.json
│   ├── cwii-provider-aws/  # AWS provider: AWS_* env-var injection
│   ├── cwii-provider-az/   # Azure provider: AZURE_* env-var injection
│   └── cwii/               # the binary: clap CLI, HTTPS server, /healthz +
│                           #   /mutate, provider registration (config.rs)
├── charts/
│   └── cwii/               # Helm chart (MutatingWebhookConfiguration, RBAC,
│                           #   Deployment, cert-manager / genSignedCert TLS)
└── docs/                   # user-facing docs (install, annotations,
                            #   self-hosted-oidc, verification, ...)
```

Each provider lives in its own crate so that each cloud's wiring is isolated; `cwii-core` knows
nothing cloud-specific beyond the `Provider` trait.

---

## How cwii works (developer mental model)

You don't need to read all of this to fix a typo, but understanding the data flow makes most changes
obvious. The authoritative user-facing references are [`./annotations.md`](./annotations.md) and
[`./self-hosted-oidc.md`](./self-hosted-oidc.md).

### The hard prerequisite: self-hosted OIDC

Workload identity federation works because the cloud STS endpoints can fetch your cluster's OIDC
discovery document over HTTPS and validate the projected ServiceAccount token. The `kube-apiserver`
must publish `/.well-known/openid-configuration` and its JWKS. Relevant flags:

| `kube-apiserver` flag | Role |
| --- | --- |
| `--service-account-issuer` | Stable HTTPS URL; becomes the token `iss` claim. |
| `--service-account-jwks-uri` | Public JWKS URL the cloud STS fetches keys from. |
| `--service-account-signing-key-file` | Private key used to sign projected tokens. |
| `--service-account-key-file` | Public key(s) for verification. |
| `--api-audiences` | Accepted audiences for the apiserver. |

Projected SA tokens are standard OIDC JWTs with `sub = system:serviceaccount:<NS>:<SA>`. The token
`aud` is set **per provider** by the projected-volume audience (see below). Full setup lives in
[`./self-hosted-oidc.md`](./self-hosted-oidc.md).

### Annotation resolution and precedence

Injection is driven entirely by `cwii.dev/`-prefixed annotations. Each key is resolved
**independently** with the precedence:

```text
pod  >  owning workload  >  ServiceAccount  >  namespace
```

The **first explicit value wins**, so a specific `"false"` suppresses a broader `"true"`, and one
provider's annotations never affect another. The owner walk handles
`ReplicaSet -> Deployment` (Deployment annotations preferred), plus `StatefulSet`, `DaemonSet` and
`Job`.

After mutation, the webhook writes the status marker `cwii.dev/injected` whose value is the
comma-joined, sorted list of provider abbreviations it injected (e.g. `"aws,gcp"`).

### Token mounting — the core multi-cloud design

Each enabled provider gets its **own** projected `serviceAccountToken` volume, because **each cloud
needs a different token `aud`**:

| Property | Value |
| --- | --- |
| Volume name | `cwii-<p>-token` |
| Mount path (read-only) | `/var/run/secrets/cwii.dev/<p>` |
| File | `token` |
| Token path | `/var/run/secrets/cwii.dev/<p>/token` |
| `expirationSeconds` | default `3600` (Kubernetes minimum `600`) |

### Per-provider wiring

**GCP** — builds a Google `external_account` `credentials.json`:

```json
{
  "type": "external_account",
  "audience": "<gcp-audience>",
  "subject_token_type": "urn:ietf:params:oauth:token-type:jwt",
  "token_url": "https://sts.googleapis.com/v1/token",
  "token_info_url": "https://sts.googleapis.com/v1/introspect",
  "credential_source": { "file": "/var/run/secrets/cwii.dev/gcp/token" }
}
```

If `cwii.dev/gcp-service-account` is set, cwii adds an impersonation URL
(`service_account_impersonation_url=https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/<GSA>:generateAccessToken`);
otherwise it uses **direct federation**. The file is mounted at
`/var/run/secrets/cwii.dev/gcp-creds/credentials.json` and `GOOGLE_APPLICATION_CREDENTIALS` points
there.

GCP credential delivery (`cwii.dev/gcp-delivery`) has two modes:

| Mode | Behaviour |
| --- | --- |
| `configMap` | Webhook server-side-applies a ConfigMap named `cwii-gcp-creds-<6 hex of sha256(audience+NUL+sa)>`, labeled `app.kubernetes.io/managed-by=cwii`, mounted via configMap volume `cwii-gcp-creds`. **Requires ConfigMap-write RBAC.** |
| `initContainer` | An `emptyDir` volume `cwii-gcp-creds` plus init container `cwii-gcp-creds-writer` (image `busybox:stable`) writes `credentials.json` from env `CWII_GCP_CREDS_JSON`. **No cluster writes.** |

**AWS** — env-vars only (no file). Mechanism: `AssumeRoleWithWebIdentity`. Token `aud` defaults to
`sts.amazonaws.com`.

| Env var | Source |
| --- | --- |
| `AWS_ROLE_ARN` | `cwii.dev/aws-role-arn` (required to inject) |
| `AWS_WEB_IDENTITY_TOKEN_FILE` | `/var/run/secrets/cwii.dev/aws/token` |
| `AWS_REGION` | `cwii.dev/aws-region` (optional) |
| `AWS_ROLE_SESSION_NAME` | `cwii.dev/aws-role-session-name` (optional) |

**Azure** — env-vars only. Mechanism: Entra ID federated identity credential. Token `aud` defaults
to `api://AzureADTokenExchange`.

| Env var | Source |
| --- | --- |
| `AZURE_CLIENT_ID` | `cwii.dev/az-client-id` (required) |
| `AZURE_TENANT_ID` | `cwii.dev/az-tenant-id` (required) |
| `AZURE_FEDERATED_TOKEN_FILE` | `/var/run/secrets/cwii.dev/az/token` |
| `AZURE_AUTHORITY_HOST` | `cwii.dev/az-authority-host` (optional) |

### Verify ("can-i") init containers

Opt-in via `cwii.dev/<p>-verify`. These run at order `10` (after the GCP writer at order `0`):

| Container | Command | Image |
| --- | --- | --- |
| `cwii-gcp-verify` | `gcloud auth application-default print-access-token` | `google/cloud-sdk:slim` |
| `cwii-aws-verify` | `aws sts get-caller-identity` | `amazon/aws-cli:latest` |
| `cwii-az-verify` | `az login --service-principal ... --federated-token ... && az account show` | `mcr.microsoft.com/azure-cli:latest` |

By default the check is **non-blocking**: it is wrapped as `<check> || echo ... >&2` so it always
exits `0` (logs only). With `cwii.dev/<p>-verify-enforce` the check runs bare, so a non-zero exit
**blocks pod startup**. Override the image via `cwii.dev/<p>-verify-image` or Helm
`providers.<p>.verifyImage`. See [`./verification.md`](./verification.md).

### The webhook itself

- `MutatingWebhookConfiguration` webhook name `mutate.cwii.dev`, matches **pods CREATE**.
- `sideEffects=NoneOnDryRun`, `reinvocationPolicy=Never`.
- `failurePolicy` defaults to `Ignore`; set it to `Fail` to **require** injection.
- The `namespaceSelector` **always** excludes the release namespace, `kube-system` and
  `kube-node-lease` (deadlock-safety — critical when `failurePolicy=Fail`).
- **TLS:** cert-manager by default (a `Certificate` + the `cert-manager.io/inject-ca-from`
  annotation; the chart does **not** set `caBundle`). Alternatively, a Helm `genSignedCert`
  self-signed fallback (`tls.certManager.enabled=false`) templates the Secret and `caBundle`
  together.

### RBAC (least privilege)

The ClusterRole reads:

- `namespaces`, `serviceaccounts` — `get`, `list`, `watch`
- `apps`: `deployments`, `statefulsets`, `daemonsets`, `replicasets`
- `batch`: `jobs`
- `configmaps`: `get`, `create`, `update`, `patch` — **only** when GCP `configMap` delivery is
  enabled.

### Server flags / env (clap)

| Flag | Env | Default |
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

Endpoints: `GET /healthz` and `POST /mutate` (both HTTPS).

---

## Adding a new provider

cwii is designed so a new cloud is a self-contained crate. The steps:

1. **Create a crate** under `crates/cwii-provider-<name>/` that implements the
   `cwii_core::Provider` trait. At minimum a provider supplies:
   - an **`id`** (the provider abbreviation used in annotations, volume names and the
     `cwii.dev/injected` marker), and
   - a **`plan`** that, given the resolved annotations, produces the mutation (its projected
     token volume, env vars / files, and any init containers).

   ```rust
   use cwii_core::Provider;

   pub struct MyProvider;

   impl Provider for MyProvider {
       fn id(&self) -> &'static str {
           "myc"
       }

       fn plan(&self, /* resolved config */) -> /* plan */ {
           // Build:
           //  - projected serviceAccountToken volume cwii-myc-token
           //    mounted at /var/run/secrets/cwii.dev/myc
           //  - env vars and/or credential files
           //  - optional verify init container at order 10
           todo!()
       }
   }
   ```

2. **Register it** in `crates/cwii/src/config.rs` so the binary wires it into provider resolution
   and exposes its `--myc-enabled` / `--myc-default-audience` / `--myc-verify-image` flags.

3. **Add Helm values + deployment flags** in `charts/cwii` (a `providers.myc.*` block, the
   corresponding server flags on the Deployment, and any RBAC the provider needs).

4. **Add docs**: update [`./annotations.md`](./annotations.md) with the new
   `cwii.dev/myc-*` keys and add provider-specific guidance.

> [!NOTE]
> Reuse the existing token-mounting convention: one projected volume `cwii-<id>-token` mounted at
> `/var/run/secrets/cwii.dev/<id>`, file `token`. Don't share a token across providers — each cloud
> needs its own audience.

---

## Project conventions

- **Annotation keys are public API.** Treat `cwii.dev/*` keys with the same care as any released
  interface. Changing or removing one is a breaking change; always update
  [`./annotations.md`](./annotations.md) in the same PR.
- **Conventional Commits.** Commit messages must follow
  [Conventional Commits](https://www.conventionalcommits.org/) — they feed `release-please`, which
  drives versioning and changelog generation.
- **Chart value changes require docs.** Any change to chart values must update
  [`./install.md`](./install.md) so the documented values stay in sync with the chart.
- **Least privilege.** Don't broaden RBAC unless a feature genuinely requires it (e.g. ConfigMap
  write is gated on GCP `configMap` delivery).
- **Keep providers isolated.** Cloud-specific logic belongs in its provider crate, not in
  `cwii-core`.

---

## Out-of-cluster testing

You can exercise the mutation logic against a real apiserver **without persisting any pods** by
registering a temporary webhook and using server-side dry-run.

1. **Generate a self-signed keypair** for the webhook server:

   ```bash
   openssl req -x509 -newkey rsa:2048 -nodes \
     -keyout tls.key -out tls.crt -days 1 \
     -subj "/CN=cwii-dev" \
     -addext "subjectAltName=IP:127.0.0.1"
   ```

2. **Run cwii locally**, pointing it at the generated cert/key:

   ```bash
   cargo run -p cwii -- \
     --addr 127.0.0.1:8443 \
     --tls-cert ./tls.crt \
     --tls-key ./tls.key
   ```

3. **Register a temporary `MutatingWebhookConfiguration`** that points at your local server. Use the
   base64 of `tls.crt` as the `caBundle` and a `url` (not a `service`) so the apiserver reaches your
   machine. For example:

   ```yaml
   apiVersion: admissionregistration.k8s.io/v1
   kind: MutatingWebhookConfiguration
   metadata:
     name: cwii-dev-temp
   webhooks:
     - name: mutate.cwii.dev
       admissionReviewVersions: ["v1"]
       sideEffects: NoneOnDryRun
       reinvocationPolicy: Never
       failurePolicy: Ignore
       clientConfig:
         url: https://127.0.0.1:8443/mutate
         caBundle: <base64 of tls.crt>
       rules:
         - apiGroups: [""]
           apiVersions: ["v1"]
           operations: ["CREATE"]
           resources: ["pods"]
       namespaceSelector:
         matchExpressions:
           - key: kubernetes.io/metadata.name
             operator: NotIn
             values: ["kube-system", "kube-node-lease"]
   ```

   ```bash
   kubectl apply -f cwii-dev-temp.yaml
   ```

4. **Exercise mutation with server-side dry-run** so nothing is persisted:

   ```bash
   kubectl apply --dry-run=server -f my-annotated-pod.yaml -o yaml
   ```

   Inspect the returned object for the injected token volumes, env vars / credential files, any
   verify init containers, and the `cwii.dev/injected` marker.

5. **Clean up** when done:

   ```bash
   kubectl delete mutatingwebhookconfiguration cwii-dev-temp
   ```

> [!WARNING]
> Always scope the temporary webhook's `namespaceSelector` to a throwaway namespace (or at least
> keep the `kube-system` / `kube-node-lease` exclusions). A misconfigured webhook with
> `failurePolicy=Fail` can wedge pod creation cluster-wide.

---

## Pull request checklist

Before requesting review, confirm:

- [ ] `cargo build --workspace` succeeds.
- [ ] `cargo test --workspace` passes.
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` is clean.
- [ ] `cargo +nightly fmt --all --check` is clean.
- [ ] `cargo deny check` passes.
- [ ] `helm lint charts/cwii` passes and `helm template charts/cwii` renders.
- [ ] Commits follow **Conventional Commits**.
- [ ] Any new/changed `cwii.dev/*` annotation is reflected in [`./annotations.md`](./annotations.md).
- [ ] Any chart value change is reflected in [`./install.md`](./install.md).
- [ ] New providers register in `crates/cwii/src/config.rs` and ship Helm values + flags + docs.
- [ ] RBAC changes stay least-privilege and are justified in the PR description.
- [ ] Docs cross-links and examples are updated where behaviour changed.

Thanks for contributing to cwii!
