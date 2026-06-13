# End-to-end testing against real clouds

This page explains how to run **full end-to-end tests** — a real Kubernetes cluster federating real
projected tokens to **real GCP, AWS and Azure** and getting real credentials back. **Yes, it runs in
GitHub Actions** (`.github/workflows/e2e-cloud.yaml`), and it needs **no cloud credentials in CI**.

## The challenge, and the trick

cwii federates a **self-hosted** cluster, so the test cluster must itself be an OIDC identity
provider that the cloud STS endpoints can reach and validate. An ephemeral kind cluster in CI is not
publicly reachable, so the clouds can't fetch its JWKS — that's the whole problem.

The trick that makes this CI-able and repeatable:

1. **A fixed ServiceAccount signing key** (stored as a GitHub secret). Every run configures kind's
   apiserver to sign tokens with this key.
2. **A stable, public issuer URL** (a small GCS bucket) hosting the OIDC discovery document + JWKS
   for that fixed key. Set up **once**.
3. **Cloud trust registered once** against that stable issuer, for fixed test subjects
   (`system:serviceaccount:e2e-<provider>:cwii-e2e`).
4. **The assertion is cwii's own `verify-enforce`**: a test pod annotated
   `cwii.dev/<p>-inject + <p>-verify + <p>-verify-enforce` only reaches `Ready` if the real
   federation succeeds (the enforcing init container runs `aws sts get-caller-identity` /
   `gcloud auth application-default print-access-token` / `az account show` and blocks otherwise).

```
GitHub Actions runner
  └─ kind cluster (apiserver issuer = https://storage.googleapis.com/<bucket>,
     │                signing key = OIDC_SIGNING_KEY secret)
     ├─ cwii webhook (installed via Helm)
     └─ test pod (cwii.dev/<p>-verify-enforce: "true")
            │ projected SA token (signed by the fixed key)
            ▼
        GCP / AWS / Azure STS  ──fetch──►  https://storage.googleapis.com/<bucket>
                                            /.well-known/openid-configuration + /openid/v1/jwks
            │ token valid (aud + sub + signature) ⇒ short-lived creds
            ▼
        verify init container exits 0 ⇒ pod Ready ⇒ test passes
```

Because the test pods authenticate themselves, **the workflow never holds cloud credentials** — only
the signing-key secret. Cloud-side trust is one-time manual setup (below).

---

## One-time setup

### 1. Generate the signing keypair

```bash
openssl genrsa -out sa.key 2048
openssl rsa -in sa.key -pubout -out sa.pub
```

Store the **private** key as a repo secret (the public JWKS is derived and published below):

```bash
gh secret set OIDC_SIGNING_KEY < sa.key   # repo: cluster-workload-identity/cwii
```

### 2. Publish the issuer (GCS)

Use a GCS bucket so you control `Content-Type: application/json` (some OIDC validators are strict).
Derive the **exact** JWKS by letting an apiserver emit it, so it always matches the key:

```bash
export BUCKET=cwii-e2e-oidc
export ISSUER="https://storage.googleapis.com/${BUCKET}"

# Bring up a throwaway kind cluster with the fixed key + final issuer URLs.
mkdir -p /tmp/oidc && cp sa.key sa.pub /tmp/oidc/
sed -e "s|__OIDC_DIR__|/tmp/oidc|g" -e "s|__ISSUER_URL__|${ISSUER}|g" \
  e2e/kind-cluster.yaml > /tmp/kind.yaml
kind create cluster --name cwii-oidc --image kindest/node:v1.30.0 --config /tmp/kind.yaml

# The apiserver now advertises ISSUER and signs with sa.key; capture what it serves.
kubectl get --raw /.well-known/openid-configuration > openid-configuration
kubectl get --raw /openid/v1/jwks > jwks.json
kind delete cluster --name cwii-oidc

# Publish, public-read, JSON content type. Path layout must match the issuer + jwks_uri.
gcloud storage buckets create "gs://${BUCKET}" --uniform-bucket-level-access
gcloud storage buckets add-iam-policy-binding "gs://${BUCKET}" \
  --member=allUsers --role=roles/storage.objectViewer
gcloud storage cp --content-type=application/json \
  openid-configuration "gs://${BUCKET}/.well-known/openid-configuration"
gcloud storage cp --content-type=application/json \
  jwks.json "gs://${BUCKET}/openid/v1/jwks"

# Sanity: these must both return JSON from outside the cluster.
curl -s "${ISSUER}/.well-known/openid-configuration" | jq .
curl -s "${ISSUER}/openid/v1/jwks" | jq .
```

!!! note "Why GCS and not cwii.dev / Pages?"
    You could host these on `cwii.dev` (GitHub Pages), but Pages can't set
    `Content-Type: application/json` on the extensionless discovery file, which some validators
    reject. GCS path-style serves over HTTPS with a controllable content type, so it's the reliable
    choice. (You already have a GCP project for this.) S3 needs CloudFront for HTTPS.

### 3. AWS trust (once)

```bash
aws iam create-open-id-connect-provider \
  --url "${ISSUER}" \
  --client-id-list "sts.amazonaws.com" \
  --thumbprint-list "$(echo | openssl s_client -servername storage.googleapis.com \
      -connect storage.googleapis.com:443 2>/dev/null | openssl x509 -fingerprint -noout -sha1 \
      | sed 's/.*=//;s/://g')"
```

Create a read-only role trusting the e2e subject (replace `ACCOUNT` and the issuer host+path):

```json
{
  "Version": "2012-10-17",
  "Statement": [{
    "Effect": "Allow",
    "Principal": { "Federated": "arn:aws:iam::ACCOUNT:oidc-provider/storage.googleapis.com/cwii-e2e-oidc" },
    "Action": "sts:AssumeRoleWithWebIdentity",
    "Condition": { "StringEquals": {
      "storage.googleapis.com/cwii-e2e-oidc:aud": "sts.amazonaws.com",
      "storage.googleapis.com/cwii-e2e-oidc:sub": "system:serviceaccount:e2e-aws:cwii-e2e"
    }}
  }]
}
```

```bash
aws iam create-role --role-name cwii-e2e --assume-role-policy-document file://trust.json
# sts:GetCallerIdentity needs no permissions; attach read-only policies for richer resource tests.
```

Record the role ARN — it becomes the `E2E_AWS_ROLE_ARN` repo variable. See [AWS setup](./aws-setup.md)
for the full reference.

### 4. GCP trust (once)

```bash
gcloud iam workload-identity-pools create cwii-e2e --location=global
gcloud iam workload-identity-pools providers create-oidc cwii-e2e \
  --location=global --workload-identity-pool=cwii-e2e \
  --issuer-uri="${ISSUER}" \
  --allowed-audiences="//iam.googleapis.com/projects/NUM/locations/global/workloadIdentityPools/cwii-e2e/providers/cwii-e2e" \
  --attribute-mapping="google.subject=assertion.sub"
```

Grant the federated principal access (direct federation shown; or use impersonation per
[GCP setup](./gcp-setup.md)):

```bash
gcloud projects add-iam-policy-binding PROJECT --role=roles/browser \
  --member="principal://iam.googleapis.com/projects/NUM/locations/global/workloadIdentityPools/cwii-e2e/subject/system:serviceaccount:e2e-gcp:cwii-e2e"
```

The provider resource string becomes the `E2E_GCP_AUDIENCE` repo variable.

### 5. Azure trust (once)

```bash
az ad app create --display-name cwii-e2e
APP_ID=$(az ad app list --display-name cwii-e2e --query '[0].appId' -o tsv)
az ad app federated-credential create --id "$APP_ID" --parameters '{
  "name": "cwii-e2e",
  "issuer": "'"${ISSUER}"'",
  "subject": "system:serviceaccount:e2e-az:cwii-e2e",
  "audiences": ["api://AzureADTokenExchange"]
}'
# Create a service principal + a read-only role assignment for resource tests.
```

`APP_ID` and your tenant id become `E2E_AZ_CLIENT_ID` / `E2E_AZ_TENANT_ID`. See
[Azure setup](./az-setup.md).

---

## GitHub repository configuration

| Kind | Name | Value |
| --- | --- | --- |
| Secret | `OIDC_SIGNING_KEY` | PEM contents of `sa.key` |
| Variable | `E2E_ISSUER_URL` | `https://storage.googleapis.com/cwii-e2e-oidc` |
| Variable | `E2E_AWS_ROLE_ARN` | the `cwii-e2e` role ARN |
| Variable | `E2E_GCP_AUDIENCE` | the WIF provider resource string |
| Variable | `E2E_GCP_SERVICE_ACCOUNT` | *(optional)* GSA email, to test impersonation |
| Variable | `E2E_AZ_CLIENT_ID` | the app registration client id |
| Variable | `E2E_AZ_TENANT_ID` | your Entra tenant id |

---

## Running it

Trigger **Actions → e2e-cloud → Run workflow** (`workflow_dispatch`), optionally narrowing
`providers` to e.g. `aws`. There's also an optional weekly schedule.

Each run: writes the signing key, renders `e2e/kind-cluster.yaml` with your issuer, creates the kind
cluster, **asserts the apiserver advertises your issuer** (fails fast otherwise), builds and loads
the cwii image, installs cert-manager + cwii, then for each selected provider creates
`namespace/e2e-<p>` + `serviceaccount/cwii-e2e` and a pod annotated with `…-verify-enforce: "true"`.
It `kubectl wait`s for `Ready` — green means the full federation worked; on failure it dumps the
verify init-container logs.

Run the exact same steps locally with `kind` + `helm` if you prefer (the workflow is a thin wrapper
around them).

---

## Security considerations

- Use **dedicated test** accounts/project/subscription, and **read-only** roles scoped to the exact
  `system:serviceaccount:e2e-<p>:cwii-e2e` subjects — never broad permissions.
- `OIDC_SIGNING_KEY` is sensitive: anyone with it can mint tokens for the e2e issuer. Its blast
  radius is bounded by those read-only, subject-scoped cloud roles. Rotate it periodically (generate
  a new key, re-publish the JWKS, the issuer URL is unchanged).
- The issuer bucket is intentionally world-readable — it contains only **public** keys and OIDC
  metadata, which is exactly what every OIDC provider exposes.

## Troubleshooting

- **apiserver advertises the wrong issuer** → the kind extraArgs didn't take. On Kubernetes ≥ 1.31
  (kubeadm `v1beta4`) `extraArgs` is a *list*, not a map; adapt `e2e/kind-cluster.yaml` accordingly.
- **`invalid_grant` / audience mismatch** → the projected token `aud` doesn't match the provider's
  allowed audience; check `E2E_GCP_AUDIENCE` / client-id lists.
- **`Unable to fetch JWKS`** → the bucket objects aren't public or lack `Content-Type: application/json`.
- **pod stuck in `Init`** → read `kubectl logs -n e2e-<p> e2e -c cwii-<p>-verify`; that's the real
  federation error.
