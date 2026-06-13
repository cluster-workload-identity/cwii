# Azure setup

This guide configures [Microsoft Entra ID](https://learn.microsoft.com/entra/identity/) workload
identity federation so that pods on your **self-hosted** Kubernetes cluster can authenticate to
Azure using their Kubernetes ServiceAccount tokens — **no client secrets, no certificates, no
static credentials**.

cwii (Cluster Workload Identity Injector) is a Rust mutating admission webhook. When a pod is
annotated for Azure, cwii injects a projected ServiceAccount token and the environment variables
that the Azure Identity SDKs read to perform an Entra ID **federated identity credential** token
exchange.

!!! abstract "How the trust works"
    1. The kube-apiserver signs a projected ServiceAccount token (an OIDC JWT) for the pod with the
       audience `api://AzureADTokenExchange`.
    2. cwii mounts that token at `/var/run/secrets/cwii.dev/az/token` and sets
       `AZURE_FEDERATED_TOKEN_FILE` to point at it.
    3. The Azure SDK presents the token to Entra ID. Entra ID validates it against the **federated
       identity credential** you registered (matching issuer, subject and audience), then returns an
       Azure access token for the app registration or user-assigned managed identity.

---

## Prerequisites

Before you start, confirm the following.

| Requirement | Detail |
| --- | --- |
| Cluster OIDC issuer published | The kube-apiserver must serve a public HTTPS OIDC discovery document and JWKS that Entra ID can fetch. See [Self-hosted OIDC setup](./self-hosted-oidc.md). |
| cwii installed | The webhook is running in the `cwii-system` namespace with Azure enabled (`--az-enabled` defaults to `true`). See [Install](./install.md). |
| `az` CLI | Azure CLI logged in with permission to create app registrations / managed identities and assign Azure RBAC roles. |
| `kubectl` | Configured against the target cluster. |

!!! warning "The issuer is the hard prerequisite"
    Entra ID fetches your cluster's JWKS over the public internet to verify token signatures. The
    value you pass to the kube-apiserver flag `--service-account-issuer` (the `iss` claim) **must**
    be reachable and **must match byte-for-byte** the issuer you register in the federated identity
    credential below. If you have not completed [Self-hosted OIDC setup](./self-hosted-oidc.md),
    stop here.

Record these values — you will reuse them throughout:

```bash
# The HTTPS URL configured as --service-account-issuer on your kube-apiserver.
export ISSUER_URL="https://oidc.example.com/my-cluster"

# The Kubernetes namespace and ServiceAccount your workload runs as.
export K8S_NAMESPACE="apps"
export K8S_SA="reports"

# Entra subject claim — see the gotcha below; this format is mandatory.
export SUBJECT="system:serviceaccount:${K8S_NAMESPACE}:${K8S_SA}"

# The audience cwii requests for Azure tokens. Do not change this.
export AUDIENCE="api://AzureADTokenExchange"
```

---

## Step 1 — Choose an identity

Entra ID supports federated identity credentials on two kinds of identity. Pick one.

=== "App registration (service principal)"

    Create (or reuse) an app registration. Its **Application (client) ID** becomes
    `cwii.dev/az-client-id`.

    ```bash
    az ad app create --display-name "cwii-reports" --query appId -o tsv
    # -> save the appId, e.g. 11111111-1111-1111-1111-111111111111
    export APP_ID="11111111-1111-1111-1111-111111111111"

    # Ensure a service principal exists for the app (needed for RBAC assignment).
    az ad sp create --id "$APP_ID"
    ```

=== "User-assigned managed identity (UAMI)"

    Create (or reuse) a UAMI. Its **Client ID** becomes `cwii.dev/az-client-id`.

    ```bash
    export RESOURCE_GROUP="rg-cwii"
    export LOCATION="eastus"

    az identity create \
      --name "cwii-reports" \
      --resource-group "$RESOURCE_GROUP" \
      --location "$LOCATION"

    export APP_ID="$(az identity show \
      --name cwii-reports --resource-group "$RESOURCE_GROUP" \
      --query clientId -o tsv)"
    ```

In both cases also record your **tenant ID**, which becomes `cwii.dev/az-tenant-id`:

```bash
export TENANT_ID="$(az account show --query tenantId -o tsv)"
```

---

## Step 2 — Add a federated identity credential

This is the trust anchor. It tells Entra ID to accept tokens whose `iss`, `sub` and `aud` claims
match exactly.

=== "App registration"

    ```bash
    az ad app federated-credential create \
      --id "$APP_ID" \
      --parameters "$(cat <<JSON
    {
      "name": "cwii-${K8S_NAMESPACE}-${K8S_SA}",
      "issuer": "${ISSUER_URL}",
      "subject": "${SUBJECT}",
      "audiences": ["${AUDIENCE}"],
      "description": "cwii workload identity for ${SUBJECT}"
    }
    JSON
    )"
    ```

=== "User-assigned managed identity"

    ```bash
    az identity federated-credential create \
      --name "cwii-${K8S_NAMESPACE}-${K8S_SA}" \
      --identity-name "cwii-reports" \
      --resource-group "$RESOURCE_GROUP" \
      --issuer "$ISSUER_URL" \
      --subject "$SUBJECT" \
      --audiences "$AUDIENCE"
    ```

!!! danger "All three claims must match exactly"
    - **`issuer`** must equal the kube-apiserver `--service-account-issuer` byte-for-byte (mind the
      trailing slash and `https://` scheme).
    - **`subject`** must be exactly `system:serviceaccount:NS:SA` — the namespace and
      ServiceAccount of the pod, not a display name.
    - **`audiences`** must contain `api://AzureADTokenExchange`, which is exactly the audience cwii
      requests for the Azure projected token.

    If any claim differs, the SDK token exchange fails with `AADSTS70021: No matching federated
    identity record found`.

---

## Step 3 — Assign Azure RBAC roles

Federation establishes *who* the workload is; RBAC establishes *what it can do*. Grant the identity
the roles it needs on the target scope.

```bash
# Example: read access to a storage account.
export ASSIGNEE_OBJECT_ID="$(az ad sp show --id "$APP_ID" --query id -o tsv)"   # app reg
# For a UAMI:
# export ASSIGNEE_OBJECT_ID="$(az identity show --name cwii-reports \
#   --resource-group "$RESOURCE_GROUP" --query principalId -o tsv)"

az role assignment create \
  --assignee-object-id "$ASSIGNEE_OBJECT_ID" \
  --assignee-principal-type ServicePrincipal \
  --role "Storage Blob Data Reader" \
  --scope "/subscriptions/<SUB_ID>/resourceGroups/${RESOURCE_GROUP}/providers/Microsoft.Storage/storageAccounts/<ACCOUNT>"
```

!!! tip "Role propagation is eventually consistent"
    New role assignments can take a minute or two to take effect. A workload may federate
    successfully (it has a valid token) yet still get `AuthorizationFailed` on the data plane until
    the assignment propagates.

---

## Step 4 — Annotate the workload

cwii activates Azure injection per workload using annotations under the `cwii.dev/` prefix. The two
required keys are the client ID and tenant ID from Steps 1–2.

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: reports
  namespace: apps
  annotations:
    cwii.dev/az-inject: "true"
    cwii.dev/az-client-id: "11111111-1111-1111-1111-111111111111"  # app/identity client ID
    cwii.dev/az-tenant-id: "22222222-2222-2222-2222-222222222222"  # Entra tenant ID
spec:
  serviceAccountName: reports
  containers:
    - name: app
      image: mcr.microsoft.com/azure-cli:latest
      command: ["sleep", "infinity"]
```

The Azure-specific annotations are:

| Annotation | Required | Description |
| --- | --- | --- |
| `cwii.dev/az-inject` | yes | Set to `"true"` to enable Azure injection (`"false"` suppresses it). |
| `cwii.dev/az-client-id` | **yes** | App registration / managed identity client ID. Injection requires it. |
| `cwii.dev/az-tenant-id` | **yes** | Entra ID tenant ID. Injection requires it. |
| `cwii.dev/az-authority-host` | no | Override the Entra authority host (e.g. for sovereign clouds). |
| `cwii.dev/az-audience` | no | Override the projected-token audience (default `api://AzureADTokenExchange`). |
| `cwii.dev/az-token-expiration` | no | Projected token lifetime in seconds (default `3600`, Kubernetes minimum `600`). |
| `cwii.dev/az-verify` | no | `"true"` adds a non-blocking `can-i` init container (see [Step 6](#step-6-verify-the-injection)). |
| `cwii.dev/az-verify-enforce` | no | `"true"` makes a failed verify **block** pod startup. |
| `cwii.dev/az-verify-image` | no | Override the verify init-container image. |

!!! note "Annotation precedence"
    cwii resolves each annotation key **independently** using the precedence
    **pod &gt; owning workload &gt; ServiceAccount &gt; namespace**. The first explicit value wins, so
    a specific `"false"` on the pod can suppress a broader `"true"` set on the namespace, and one
    provider never affects another. The owner walk resolves `ReplicaSet -> Deployment` (Deployment
    annotations preferred) as well as `StatefulSet`, `DaemonSet` and `Job`. See the
    [Annotations reference](./annotations.md) for the full model.

    In practice you usually annotate the **ServiceAccount** so every pod that runs as it inherits
    the federation config:

    ```yaml
    apiVersion: v1
    kind: ServiceAccount
    metadata:
      name: reports
      namespace: apps
      annotations:
        cwii.dev/az-inject: "true"
        cwii.dev/az-client-id: "11111111-1111-1111-1111-111111111111"
        cwii.dev/az-tenant-id: "22222222-2222-2222-2222-222222222222"
    ```

---

## Step 5 — What cwii injects

When the webhook admits the pod, it mutates the spec and writes the status marker
`cwii.dev/injected` with the comma-joined sorted provider abbreviations it acted on (for example
`az`, or `aws,az,gcp` for a multi-cloud pod).

### Environment variables

Azure uses an **env-vars-only** mechanism — there is no credentials file. cwii adds these to every
container in the pod:

| Variable | Value |
| --- | --- |
| `AZURE_CLIENT_ID` | from `cwii.dev/az-client-id` |
| `AZURE_TENANT_ID` | from `cwii.dev/az-tenant-id` |
| `AZURE_FEDERATED_TOKEN_FILE` | `/var/run/secrets/cwii.dev/az/token` |
| `AZURE_AUTHORITY_HOST` | from `cwii.dev/az-authority-host` (only if set) |

These are precisely the variables the Azure Identity SDKs (`WorkloadIdentityCredential` /
`DefaultAzureCredential`) read to perform the federated token exchange via the Entra ID mechanism.

### Projected token volume

cwii gives each enabled provider its **own** projected ServiceAccount token volume, because every
cloud requires a different token audience. For Azure:

```yaml
# Injected by cwii into the pod spec.
volumes:
  - name: cwii-az-token
    projected:
      sources:
        - serviceAccountToken:
            path: token
            audience: api://AzureADTokenExchange
            expirationSeconds: 3600   # cwii.dev/az-token-expiration; min 600
```

```yaml
# Injected into every container.
volumeMounts:
  - name: cwii-az-token
    mountPath: /var/run/secrets/cwii.dev/az
    readOnly: true
```

The token file therefore lands at `/var/run/secrets/cwii.dev/az/token`, exactly where
`AZURE_FEDERATED_TOKEN_FILE` points.

!!! info "Why a dedicated volume per provider"
    A projected ServiceAccount token is minted for a single audience. Azure requires
    `api://AzureADTokenExchange`, AWS requires `sts.amazonaws.com`, and GCP requires its STS
    audience. cwii mounts a separate `cwii-<p>-token` volume per provider so a multi-cloud pod
    holds the correct, distinct token for each.

---

## Step 6 — Verify the injection

Set `cwii.dev/az-verify: "true"` to have cwii add a `can-i` init container named `cwii-az-verify`.
It runs (using the image `mcr.microsoft.com/azure-cli:latest` by default):

```bash
az login --service-principal ... --federated-token ... && az account show
```

```yaml
metadata:
  annotations:
    cwii.dev/az-inject: "true"
    cwii.dev/az-client-id: "11111111-1111-1111-1111-111111111111"
    cwii.dev/az-tenant-id: "22222222-2222-2222-2222-222222222222"
    cwii.dev/az-verify: "true"
```

| Mode | Annotation | Behaviour |
| --- | --- | --- |
| Non-blocking (default) | `cwii.dev/az-verify: "true"` | The check is wrapped as `<check> \|\| echo ... >&2`, so it always exits `0`. Failures are logged only and do **not** block startup. |
| Enforcing | `cwii.dev/az-verify-enforce: "true"` | The check runs bare. A non-zero exit **blocks pod startup** — useful for fail-fast rollouts. |

Override the image per workload with `cwii.dev/az-verify-image`, or cluster-wide via the Helm value
`providers.az.verifyImage` (server flag `--az-verify-image`). See [Verification](./verification.md)
for details and the ordering of init containers (verify runs at order `10`).

Quick manual check once the pod is running:

```bash
kubectl -n apps exec deploy/reports -- env | grep AZURE_
kubectl -n apps exec deploy/reports -- \
  cat /var/run/secrets/cwii.dev/az/token | cut -d. -f2 | base64 -d 2>/dev/null
# Inspect the decoded JWT body: aud should be api://AzureADTokenExchange,
# iss your cluster issuer, sub system:serviceaccount:apps:reports.
```

---

## Gotchas

!!! danger "Common failure modes"
    - **`AADSTS70021` / no matching federated identity record** — the `issuer`, `subject` or
      `audiences` in your federated credential do not match the token. Re-check all three (Step 2).
    - **Subject format** — the `subject` must be exactly `system:serviceaccount:NS:SA`. A typo in
      the namespace or ServiceAccount name silently breaks the match.
    - **Issuer mismatch** — the federated credential `issuer` must equal the kube-apiserver
      `--service-account-issuer` exactly, including scheme and any trailing slash.
    - **Audience** — must be `api://AzureADTokenExchange`. If you override
      `cwii.dev/az-audience`, you must register the same value in the federated credential's
      `audiences`.
    - **Federated credential limits** — Entra ID caps the number of federated identity credentials
      per app registration / managed identity. Reuse one identity across many ServiceAccounts only
      up to that limit; beyond it, split workloads across multiple identities.
    - **Missing required annotations** — Azure injection requires **both** `cwii.dev/az-client-id`
      and `cwii.dev/az-tenant-id`. Without them cwii will not inject Azure for the pod.
    - **RBAC propagation delay** — see Step 3; a freshly assigned role can lag by a minute or two.

---

## See also

- [Self-hosted OIDC setup](./self-hosted-oidc.md) — publishing the cluster issuer and JWKS (hard prerequisite).
- [Annotations reference](./annotations.md) — full annotation list and precedence model.
- [Verification](./verification.md) — `can-i` init containers and enforcement.
- [Install](./install.md) — deploying the webhook into `cwii-system`.
