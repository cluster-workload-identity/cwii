# Self-hosted Kubernetes as an OIDC identity provider

This is the hard prerequisite for everything else in cwii (Cluster Workload
Identity Injector). Before any pod can exchange its Kubernetes ServiceAccount
token for GCP, AWS or Azure credentials, the cloud providers must be able to
**independently verify** that token. They do that exactly the way any OIDC
relying party validates a JWT: by fetching your cluster's OIDC **discovery
document** and **JWKS** over public HTTPS and checking the token's signature
and claims.

If you take away one sentence: **your kube-apiserver must act as a public OIDC
identity provider whose discovery document and signing keys are reachable from
the public internet over HTTPS.** No keys, no federation.

For the cwii-specific injection behavior (annotations, token mounts, env vars)
once OIDC is working, see [Annotations reference](./annotations.md),
[Verification](./verification.md), and [Install](./install.md).

---

## Why the cloud STS must reach your cluster

cwii never ships static cloud keys. Instead each enabled provider gets its own
[projected ServiceAccount token](./annotations.md) — a standard OIDC JWT signed
by your cluster's ServiceAccount signing key. The pod presents that JWT to the
provider's Security Token Service (STS), which trades it for short-lived cloud
credentials via workload identity federation.

For that trade to succeed, the STS endpoint (Google `sts.googleapis.com`, AWS
`sts.amazonaws.com`, or Microsoft Entra ID) must **validate the JWT
cryptographically**. It cannot do that with a secret you share — federation is
keyless. It does it by following the OIDC discovery chain:

```text
projected SA token (JWT)
  │  header.kid + payload.iss + payload.aud + payload.sub + payload.exp
  ▼
iss claim ─────────────────────────────► <issuer>/.well-known/openid-configuration
                                                  │ contains "jwks_uri"
                                                  ▼
jwks_uri ──────────────────────────────► <jwks-uri>  (public JWKS, the signing public keys)
                                                  │ pick key by "kid"
                                                  ▼
verify JWT signature, then check:
  • aud  == the provider's expected audience  (allow-listed cloud-side)
  • sub  == system:serviceaccount:<namespace>:<serviceaccount>
  • exp  not expired   (clock-skew sensitive — keep NTP in sync)
```

!!! important
    Both the discovery document and the JWKS must be served over **public
    HTTPS** with a publicly trusted certificate. The cloud STS endpoints run
    outside your network; they cannot reach a private API server, a `.local`
    DNS name, an internal CA, or a plain-HTTP endpoint. If either URL is
    unreachable, every federation request fails with an opaque "could not
    verify token" / "invalid identity token" error.

The audience (`aud`) is what makes cwii multi-cloud: each cloud requires a
**different** audience, so cwii mounts a **separate** projected token per
provider, each minted for that provider's audience. See the
[audience model](#audience-model) below.

---

## kube-apiserver flags

The OIDC behavior is controlled entirely by ServiceAccount-related flags on the
kube-apiserver. These are upstream Kubernetes flags — cwii does not change or
read them — but cwii cannot function unless they are set correctly.

| Flag | Purpose |
| --- | --- |
| `--service-account-issuer` | The **stable HTTPS URL** that becomes the `iss` claim of every projected token. The cloud STS appends `/.well-known/openid-configuration` to this value to discover your provider. Must be a durable, public HTTPS URL — changing it invalidates every existing federation trust. |
| `--service-account-jwks-uri` | The **public JWKS URL** advertised as `jwks_uri` inside the discovery document. Set this when the JWKS is served somewhere other than the issuer host (for example a public bucket/CDN), so discovery points the STS at the reachable location rather than the private API server. |
| `--service-account-signing-key-file` | The **private key** the API server uses to sign projected tokens. The matching public key must be published in the JWKS before this key is used (see [key rotation](#troubleshooting)). |
| `--service-account-key-file` | The **public key(s)** used to verify ServiceAccount tokens. May be specified multiple times; all listed public keys are published in the JWKS, which is what lets you stage a new key before cutting over the signing key. |
| `--api-audiences` | The set of audiences the API server will accept on inbound tokens. Projected volumes request a provider-specific `aud` (see below); ensure your audiences configuration does not reject those requests. |

!!! note
    The token `aud` for federation is **not** set by `--api-audiences`. It is
    set per provider by the projected-volume `audience` that cwii configures on
    each `cwii-<p>-token` volume (overridable with `cwii.dev/<p>-audience`).
    `--api-audiences` governs which audiences the API server is willing to mint
    and accept.

### kubeadm example

With kubeadm, set these under `apiServer.extraArgs` in your
`ClusterConfiguration`. In this example the issuer points at a public bucket
(see [Option A](#option-a-public-gcs-bucket)) while the keys are served from the
same public location:

```yaml
apiVersion: kubeadm.k8s.io/v1beta4
kind: ClusterConfiguration
apiServer:
  extraArgs:
    - name: service-account-issuer
      value: https://storage.googleapis.com/my-cluster-oidc
    - name: service-account-jwks-uri
      value: https://storage.googleapis.com/my-cluster-oidc/keys.json
    - name: service-account-signing-key-file
      value: /etc/kubernetes/pki/sa.key
    - name: service-account-key-file
      value: /etc/kubernetes/pki/sa.pub
    - name: api-audiences
      value: https://kubernetes.default.svc
```

!!! warning
    On `kubeadm.k8s.io/v1beta3` and earlier, `extraArgs` is a map
    (`service-account-issuer: https://...`) rather than the list-of-name/value
    form shown above (`v1beta4`). Use the schema that matches your kubeadm
    version. After editing static pod manifests in
    `/etc/kubernetes/manifests/kube-apiserver.yaml`, the kubelet restarts the
    API server automatically.

---

## Inspect the current discovery document and JWKS

Before publishing anything, confirm what your API server currently advertises.
These two read-only endpoints are the OIDC provider surface:

```bash
# The OIDC discovery document — note the "issuer" and "jwks_uri" values
kubectl get --raw /.well-known/openid-configuration | jq

# The JWKS — the public signing keys, keyed by "kid"
kubectl get --raw /openid/v1/jwks | jq
```

A healthy discovery document looks like this:

```json
{
  "issuer": "https://storage.googleapis.com/my-cluster-oidc",
  "jwks_uri": "https://storage.googleapis.com/my-cluster-oidc/keys.json",
  "response_types_supported": ["id_token"],
  "subject_types_supported": ["public"],
  "id_token_signing_alg_values_supported": ["RS256"]
}
```

!!! important
    The `issuer` here **must** byte-for-byte match the `iss` claim in your
    projected tokens and the issuer/provider URL you register on each cloud —
    including the scheme and **no trailing slash** unless you registered it
    with one. A mismatched trailing slash is the single most common federation
    failure.

### Allow anonymous discovery

Cloud STS endpoints fetch the discovery document **unauthenticated**. The
in-cluster `kubectl get --raw` calls above work because you are authenticated;
external anonymous callers need the built-in
`system:service-account-issuer-discovery` ClusterRole bound to
`system:unauthenticated`:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: service-account-issuer-discovery-unauth
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: system:service-account-issuer-discovery
subjects:
  - apiGroup: rbac.authorization.k8s.io
    kind: Group
    name: system:unauthenticated
```

```bash
kubectl apply -f service-account-issuer-discovery-unauth.yaml
```

!!! note
    This only matters when the cloud fetches discovery **directly from the API
    server**. If you front the two read-only paths with a public bucket or CDN
    (recommended, and required when the API server is private), the STS never
    talks to the API server and this binding is unnecessary for federation.

---

## Publishing discovery + JWKS when the API server is private

Most self-hosted clusters do **not** expose the kube-apiserver to the public
internet — and they should not. The fix is to publish copies of just the two
read-only documents (`openid-configuration` and the JWKS) at a stable public
HTTPS location, then point `--service-account-issuer` /
`--service-account-jwks-uri` at that location.

You are publishing **public, non-secret** data: the discovery document and the
**public** signing keys. Never publish the private signing key.

### Option A: public GCS bucket

A GCS bucket served over `https://storage.googleapis.com` is publicly trusted
HTTPS and needs no CDN.

```bash
# 1. Create a uniformly-public bucket
gsutil mb -b on -l us-central1 gs://my-cluster-oidc

# 2. Grant anonymous read
gsutil iam ch allUsers:objectViewer gs://my-cluster-oidc

# 3. Export the two documents from the cluster
kubectl get --raw /.well-known/openid-configuration > openid-configuration
kubectl get --raw /openid/v1/jwks                  > keys.json

# 4. Upload with the correct Content-Type
#    discovery goes under the .well-known/ path
gsutil -h "Content-Type:application/json" \
  cp openid-configuration gs://my-cluster-oidc/.well-known/openid-configuration
gsutil -h "Content-Type:application/json" \
  cp keys.json gs://my-cluster-oidc/keys.json
```

The issuer then becomes `https://storage.googleapis.com/my-cluster-oidc`, and
the discovery document must advertise
`"jwks_uri": "https://storage.googleapis.com/my-cluster-oidc/keys.json"`. Set
`--service-account-issuer` to the bucket URL and `--service-account-jwks-uri`
to the `keys.json` URL so the API server rewrites `jwks_uri` accordingly.

!!! important
    Before exporting `openid-configuration`, set the API server flags so the
    `issuer` and `jwks_uri` inside the document already point at the public
    bucket. If you export it while it still references the private API server,
    the STS will read the private `jwks_uri` from discovery and fail.

### Option B: S3 + CloudFront

An S3 static-website endpoint is **HTTP-only**, which the STS will reject. Put
CloudFront in front of the bucket to terminate HTTPS with a publicly trusted
certificate.

```bash
# Create a private bucket (CloudFront reads it via OAC, not public website hosting)
aws s3api create-bucket --bucket my-cluster-oidc \
  --region us-east-1

# Upload the two documents with JSON content type
aws s3 cp openid-configuration \
  s3://my-cluster-oidc/.well-known/openid-configuration \
  --content-type application/json
aws s3 cp keys.json \
  s3://my-cluster-oidc/keys.json \
  --content-type application/json
```

Front the bucket with a CloudFront distribution (origin = the S3 bucket via an
Origin Access Control) and use the distribution's HTTPS domain (or a custom
domain with an ACM certificate) as the issuer:
`https://oidc.example.com`. The discovery document's `jwks_uri` must point at
`https://oidc.example.com/keys.json`.

### Option C: reverse-proxy the two read-only paths

If you already run a public HTTPS ingress/CDN, you can proxy **only** these two
paths straight through to the API server's read-only OIDC endpoints, exposing
nothing else:

| Public path | Upstream (API server) |
| --- | --- |
| `/.well-known/openid-configuration` | `https://<apiserver>/.well-known/openid-configuration` |
| `/openid/v1/jwks` (or `/keys.json`) | `https://<apiserver>/openid/v1/jwks` |

Lock the proxy to GET-only on exactly these paths. The issuer becomes your
public proxy URL, and `--service-account-jwks-uri` should match whichever JWKS
path you expose. This avoids copying keys at the cost of exposing a narrow API
server surface.

### Keep the JWKS in sync (CronJob)

Options A and B serve **static copies** of the JWKS. When the API server's
signing keys rotate, those copies go stale and federation breaks. Run a CronJob
that re-publishes the documents on a schedule (for a GCS bucket):

```yaml
apiVersion: batch/v1
kind: CronJob
metadata:
  name: oidc-jwks-sync
  namespace: cwii-system
spec:
  schedule: "*/15 * * * *"
  concurrencyPolicy: Forbid
  jobTemplate:
    spec:
      template:
        spec:
          serviceAccountName: oidc-publisher
          restartPolicy: OnFailure
          containers:
            - name: sync
              image: google/cloud-sdk:slim
              command:
                - /bin/sh
                - -c
                - |
                  set -euo pipefail
                  TOKEN=$(cat /var/run/secrets/kubernetes.io/serviceaccount/token)
                  APISERVER=https://kubernetes.default.svc
                  curl -sS --cacert /var/run/secrets/kubernetes.io/serviceaccount/ca.crt \
                    -H "Authorization: Bearer ${TOKEN}" \
                    "${APISERVER}/.well-known/openid-configuration" > /tmp/openid-configuration
                  curl -sS --cacert /var/run/secrets/kubernetes.io/serviceaccount/ca.crt \
                    -H "Authorization: Bearer ${TOKEN}" \
                    "${APISERVER}/openid/v1/jwks" > /tmp/keys.json
                  gsutil -h "Content-Type:application/json" \
                    cp /tmp/openid-configuration gs://my-cluster-oidc/.well-known/openid-configuration
                  gsutil -h "Content-Type:application/json" \
                    cp /tmp/keys.json gs://my-cluster-oidc/keys.json
```

!!! warning "Rotation ordering"
    A periodic sync handles **additive** key publication automatically, but
    only if the new public key is published **before** the cluster starts
    signing with it. Stage the new key via `--service-account-key-file` (so it
    appears in the JWKS), let the sync run, and only then switch
    `--service-account-signing-key-file`. See [Troubleshooting](#troubleshooting).

---

## Audience model

Each enabled provider gets its own projected `serviceAccountToken` volume named
`cwii-<p>-token`, mounted read-only at `/var/run/secrets/cwii.dev/<p>`, with the
token file at `/var/run/secrets/cwii.dev/<p>/token`. Providers mount separately
**precisely because each cloud requires a different token audience** — the same
JWT cannot satisfy GCP and AWS at once. (`expirationSeconds` defaults to 3600,
minimum 600; override per provider with `cwii.dev/<p>-token-expiration`.)

| Provider (`<p>`) | Token mount path | Default audience | Where it must be allow-listed cloud-side |
| --- | --- | --- | --- |
| `gcp` | `/var/run/secrets/cwii.dev/gcp/token` | The Workload Identity Federation provider **resource string** (`//iam.googleapis.com/projects/<num>/locations/global/workloadIdentityPools/<pool>/providers/<provider>`) — set via `cwii.dev/gcp-audience` or `--gcp-default-audience` | The **allowed audiences** of the WIF provider, and the OIDC provider's **issuer URI** in the pool |
| `aws` | `/var/run/secrets/cwii.dev/aws/token` | `sts.amazonaws.com` | The IAM OIDC identity provider's **audience (client ID)** list, and the role trust policy `Condition` on `<issuer>:aud` |
| `az` | `/var/run/secrets/cwii.dev/az/token` | `api://AzureADTokenExchange` | The **Federated Identity Credential** `audiences` on the Entra app/user-assigned managed identity |

Override any of these per workload with `cwii.dev/<p>-audience`, or change the
server defaults with `--gcp-default-audience` / `--aws-default-audience` /
`--az-default-audience`. The `sub` claim is always
`system:serviceaccount:<namespace>:<serviceaccount>` and is what you match in
the cloud-side trust/subject condition.

See the per-provider trust setup in [GCP setup](./gcp-setup.md),
[AWS setup](./aws-setup.md), and [Azure setup](./az-setup.md).

---

## Verify a live token from the outside

Confirm end to end that a real projected token validates against your
**public** discovery + JWKS — exactly the path the cloud STS takes.

### 1. Decode a live projected token from a pod

Pick any pod that cwii has injected (it carries the `cwii.dev/injected`
status annotation) and read one of its provider tokens:

```bash
# List a pod's injected providers (comma-joined sorted abbreviations, e.g. "aws,gcp")
kubectl get pod my-pod -o jsonpath='{.metadata.annotations.cwii\.dev/injected}'

# Read the GCP token and decode its claims
TOKEN=$(kubectl exec my-pod -- cat /var/run/secrets/cwii.dev/gcp/token)

# Decode the JWT payload (base64url) without verifying — just to read claims
echo "$TOKEN" | cut -d. -f2 | tr '_-' '/+' | base64 -d 2>/dev/null | jq
```

You should see `iss` equal to your public issuer, `sub` equal to
`system:serviceaccount:<ns>:<sa>`, the provider-specific `aud`, and an `exp`.

### 2. Fetch discovery + JWKS from outside the cluster

From a machine that is **not** inside the cluster (a laptop on the public
internet — this is the cloud STS's vantage point), follow the chain:

```bash
ISSUER=https://storage.googleapis.com/my-cluster-oidc

# Discovery must be reachable, JSON, and self-consistent
curl -fsSL "${ISSUER}/.well-known/openid-configuration" | jq

# Pull the jwks_uri straight out of discovery and fetch it
JWKS_URI=$(curl -fsSL "${ISSUER}/.well-known/openid-configuration" | jq -r .jwks_uri)
curl -fsSL "${JWKS_URI}" | jq
```

!!! note
    Cross-check that the `iss` decoded in step 1 equals `ISSUER`, that the JWT
    header `kid` (`echo "$TOKEN" | cut -d. -f1 | tr '_-' '/+' | base64 -d | jq -r .kid`)
    is present in the JWKS, and that `aud` matches what you allow-listed
    cloud-side. If all three line up and the JWKS is publicly fetchable over
    HTTPS, the cloud STS can validate the token.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| `Invalid value for "audience"` / `invalid_request` from STS | Token `aud` does not match the audience allow-listed on the cloud side | Align the projected audience (`cwii.dev/<p>-audience` / `--<p>-default-audience`) with the WIF provider / IAM OIDC provider / Entra federated credential audience. See the [audience model](#audience-model). |
| STS returns "could not fetch / unreachable JWKS" | `jwks_uri` points at a private or HTTP-only endpoint | Publish the JWKS at a public HTTPS location and set `--service-account-jwks-uri` (and the `jwks_uri` in published discovery) to it. See [publishing](#publishing-discovery--jwks-when-the-api-server-is-private). |
| "issuer mismatch" / "unexpected issuer" | `iss` claim, published `issuer`, and cloud-registered issuer disagree — often a stray trailing slash | Make all three byte-identical, including scheme and trailing-slash handling. Re-export discovery after fixing `--service-account-issuer`. |
| Intermittent "token expired" / "token used before issued" | Clock skew between API server and STS | Run NTP on control-plane nodes; keep `exp`/`iat` honest. Increase `cwii.dev/<p>-token-expiration` only as a stopgap (min 600s). |
| STS refuses the discovery URL outright | Endpoint served over HTTP, with a private CA, or behind auth | Serve discovery + JWKS over **public HTTPS with a publicly trusted certificate**; allow anonymous GET (the [discovery RBAC binding](#allow-anonymous-discovery) or a public bucket/CDN). |
| Federation breaks right after key rotation | Signing key switched before the new public key was published | **Publish the new public key in the JWKS BEFORE switching the signing key.** Add it via `--service-account-key-file`, let the [sync CronJob](#keep-the-jwks-in-sync-cronjob) run, confirm the new `kid` is in the public JWKS, then change `--service-account-signing-key-file`. |

---

## Next steps

1. Confirm your public discovery + JWKS validate a live token using the
   [recipe above](#verify-a-live-token-from-the-outside).
2. Register your issuer and per-provider audiences on each cloud:
   [GCP](./gcp-setup.md) · [AWS](./aws-setup.md) · [Azure](./az-setup.md).
3. [Install cwii](./install.md) into the `cwii-system` namespace.
4. Annotate workloads and turn on per-provider injection — see the
   [Annotations reference](./annotations.md).
5. Use the opt-in `can-i` checks to confirm pods can actually authenticate —
   see [Verification](./verification.md).
