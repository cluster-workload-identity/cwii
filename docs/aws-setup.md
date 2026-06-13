# AWS setup

This guide configures **cwii** to let pods on a **self-hosted** Kubernetes cluster
authenticate to AWS using their Kubernetes ServiceAccount tokens — no static
`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` anywhere.

It is the IRSA-style pattern you may know from EKS, but driven by **your own**
cluster's OIDC issuer rather than the EKS-managed one. The underlying STS
mechanism is [`AssumeRoleWithWebIdentity`](https://docs.aws.amazon.com/STS/latest/APIReference/API_AssumeRoleWithWebIdentity.html):
the AWS SDK reads a projected ServiceAccount token from disk, exchanges it at
`sts.amazonaws.com` for temporary credentials, and proceeds with those.

!!! note "What cwii actually injects for AWS"
    AWS injection is **environment-variables only** — there is no credentials
    file to write. When a pod is mutated, cwii adds the standard web-identity
    env vars (`AWS_ROLE_ARN`, `AWS_WEB_IDENTITY_TOKEN_FILE`, and optionally
    `AWS_REGION` / `AWS_ROLE_SESSION_NAME`) plus a dedicated projected token
    volume. The AWS SDK and CLI pick these up automatically.

---

## Prerequisites

| Requirement | Detail |
| --- | --- |
| Cluster OIDC issuer published | The kube-apiserver must serve a public HTTPS `/.well-known/openid-configuration` and JWKS that AWS STS can fetch. See [Self-hosted OIDC setup](./self-hosted-oidc.md). |
| cwii installed | The webhook must be running in the `cwii-system` namespace with `--aws-enabled` (the default). See [Install](./install.md). |
| AWS account access | Permissions to create an IAM OIDC identity provider and IAM roles. |
| `aws` CLI v2 | For the setup commands below and for the optional verify init container. |

!!! warning "The issuer is a hard prerequisite"
    AWS STS validates the projected token by fetching your cluster's JWKS over
    the public internet. If `--service-account-jwks-uri` is not reachable by
    AWS, `AssumeRoleWithWebIdentity` fails with `InvalidIdentityToken`. Confirm
    the issuer is reachable **before** continuing:

    ```bash
    ISSUER="https://oidc.example.com/my-cluster"   # == kube-apiserver --service-account-issuer
    curl -fsSL "${ISSUER}/.well-known/openid-configuration" | jq .
    ```

Throughout this guide we use:

```bash
ISSUER="https://oidc.example.com/my-cluster"   # kube-apiserver --service-account-issuer (the iss claim)
ISSUER_HOST_PATH="oidc.example.com/my-cluster" # the issuer WITHOUT the https:// scheme
ACCOUNT_ID="123456789012"
AWS_REGION="us-east-1"
```

---

## Step 1 — Create the IAM OIDC identity provider

Register your cluster's issuer as an OIDC identity provider in IAM. The
`--client-id-list` becomes the set of valid token **audiences**; cwii's AWS
token uses the audience `sts.amazonaws.com`, so that must be present.

### Compute the TLS thumbprint

AWS requires a `--thumbprint-list`: the SHA-1 fingerprint of the **root** CA
certificate that terminates TLS for your issuer host.

!!! info "The thumbprint is largely vestigial"
    For OIDC providers, AWS validates the token by fetching your JWKS over a
    standard TLS-verified connection — it does **not** actually pin against the
    thumbprint for the `https`-fronted JWKS flow. The field is nonetheless
    **required** by the API, so you must supply a syntactically valid value.

Grab the certificate chain and fingerprint the **last** (root) certificate:

```bash
# Dump the server's certificate chain
echo | openssl s_client -servername oidc.example.com \
    -connect oidc.example.com:443 -showcerts 2>/dev/null \
    > /tmp/oidc-chain.pem

# Split the chain into individual certs, then fingerprint the LAST one (the root)
csplit -z -f /tmp/oidc-cert- /tmp/oidc-chain.pem '/-----BEGIN CERTIFICATE-----/' '{*}' >/dev/null
LAST_CERT=$(ls /tmp/oidc-cert-* | tail -n1)
THUMBPRINT=$(openssl x509 -in "$LAST_CERT" -noout -fingerprint -sha1 \
    | sed 's/.*=//; s/://g')

echo "$THUMBPRINT"
```

### Create the provider

```bash
aws iam create-open-id-connect-provider \
    --url "$ISSUER" \
    --client-id-list sts.amazonaws.com \
    --thumbprint-list "$THUMBPRINT"
```

Note the returned provider ARN — you will reference it in the trust policy:

```
arn:aws:iam::123456789012:oidc-provider/oidc.example.com/my-cluster
```

!!! tip "URL must match the issuer exactly"
    The `--url` you pass here must be byte-for-byte the kube-apiserver's
    `--service-account-issuer` (the `iss` claim in the token). A trailing-slash
    or path mismatch causes STS to reject the token.

---

## Step 2 — Create the IAM role and trust policy

Create an IAM role whose **trust policy** allows `sts:AssumeRoleWithWebIdentity`
from the OIDC provider, scoped to a specific ServiceAccount via condition keys.

The two condition keys are derived from the issuer **host + path, without the
`https://` scheme**:

- `<issuer-host-and-path>:aud` — must equal a client ID on the provider (here `sts.amazonaws.com`).
- `<issuer-host-and-path>:sub` — must equal the token's subject, which is
  exactly `system:serviceaccount:<NAMESPACE>:<SERVICEACCOUNT>`.

### Trust policy (single ServiceAccount)

`trust-policy.json` — binds the role to `default/my-app`:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Principal": {
        "Federated": "arn:aws:iam::123456789012:oidc-provider/oidc.example.com/my-cluster"
      },
      "Action": "sts:AssumeRoleWithWebIdentity",
      "Condition": {
        "StringEquals": {
          "oidc.example.com/my-cluster:aud": "sts.amazonaws.com",
          "oidc.example.com/my-cluster:sub": "system:serviceaccount:default:my-app"
        }
      }
    }
  ]
}
```

### Trust policy (whole namespace, wildcard)

To allow any ServiceAccount in a namespace, keep `aud` as `StringEquals` and
match `sub` with `StringLike`:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Principal": {
        "Federated": "arn:aws:iam::123456789012:oidc-provider/oidc.example.com/my-cluster"
      },
      "Action": "sts:AssumeRoleWithWebIdentity",
      "Condition": {
        "StringEquals": {
          "oidc.example.com/my-cluster:aud": "sts.amazonaws.com"
        },
        "StringLike": {
          "oidc.example.com/my-cluster:sub": "system:serviceaccount:default:*"
        }
      }
    }
  ]
}
```

### Create the role and attach permissions

```bash
aws iam create-role \
    --role-name cwii-my-app \
    --assume-role-policy-document file://trust-policy.json

# Attach whatever the workload actually needs (example: read-only S3)
aws iam attach-role-policy \
    --role-name cwii-my-app \
    --policy-arn arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess
```

The resulting role ARN is what cwii needs:

```
arn:aws:iam::123456789012:role/cwii-my-app
```

---

## Step 3 — Annotate the workload

Set [`cwii.dev/aws-role-arn`](./annotations.md) to the role ARN. This annotation
is **required** — without it, cwii performs no AWS injection even if
`cwii.dev/aws-inject` is `"true"`.

| Annotation | Required | Effect |
| --- | --- | --- |
| `cwii.dev/aws-role-arn` | **Yes** | Role to assume; sets `AWS_ROLE_ARN`. Its presence is what triggers AWS injection. |
| `cwii.dev/aws-inject` | No | `"true"`/`"false"` to explicitly enable/disable. |
| `cwii.dev/aws-region` | No | Sets `AWS_REGION`. |
| `cwii.dev/aws-role-session-name` | No | Sets `AWS_ROLE_SESSION_NAME`. |
| `cwii.dev/aws-audience` | No | Override the projected-token audience (default `sts.amazonaws.com`). Must match a client ID on the provider. |
| `cwii.dev/aws-token-expiration` | No | Projected token lifetime in seconds (Kubernetes minimum 600, default 3600). |
| `cwii.dev/aws-verify` | No | Add a non-blocking `aws sts get-caller-identity` init container. |
| `cwii.dev/aws-verify-enforce` | No | Make a failed verify **block** pod startup. |
| `cwii.dev/aws-verify-image` | No | Override the verify init-container image. |

!!! note "Annotation precedence"
    Annotations resolve **pod > owning workload > ServiceAccount > namespace**,
    evaluated independently per key. The first explicit value wins, so a
    specific `"false"` suppresses a broader `"true"`. The owner walk follows
    ReplicaSet → Deployment (Deployment annotations preferred), plus
    StatefulSet/DaemonSet/Job. See [Annotations reference](./annotations.md).

### Example: annotated Pod

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: aws-demo
  namespace: default
  annotations:
    cwii.dev/aws-role-arn: "arn:aws:iam::123456789012:role/cwii-my-app"
    cwii.dev/aws-region: "us-east-1"
    cwii.dev/aws-role-session-name: "aws-demo"
spec:
  serviceAccountName: my-app   # must match the trust policy sub
  containers:
    - name: app
      image: amazon/aws-cli:latest
      command: ["sleep", "infinity"]
```

!!! tip "Annotate the Deployment, not the Pod template caveat"
    Because cwii walks owner references, annotating a `Deployment`'s
    `spec.template.metadata.annotations` (or the Deployment itself) is the
    normal pattern — every Pod it creates inherits the configuration. The
    `serviceAccountName` you set must match the `:sub` in the trust policy.

---

## What cwii injects

After mutation, the Pod carries a projected token volume and the AWS web-identity
env vars. The injected (effective) spec looks like this:

```yaml
spec:
  serviceAccountName: my-app
  volumes:
    - name: cwii-aws-token
      projected:
        sources:
          - serviceAccountToken:
              path: token
              audience: sts.amazonaws.com
              expirationSeconds: 3600
  containers:
    - name: app
      image: amazon/aws-cli:latest
      env:
        - name: AWS_ROLE_ARN
          value: "arn:aws:iam::123456789012:role/cwii-my-app"
        - name: AWS_WEB_IDENTITY_TOKEN_FILE
          value: "/var/run/secrets/cwii.dev/aws/token"
        - name: AWS_REGION
          value: "us-east-1"
        - name: AWS_ROLE_SESSION_NAME
          value: "aws-demo"
      volumeMounts:
        - name: cwii-aws-token
          mountPath: /var/run/secrets/cwii.dev/aws
          readOnly: true
```

| What | Value |
| --- | --- |
| Projected volume name | `cwii-aws-token` |
| Mount path (read-only) | `/var/run/secrets/cwii.dev/aws` |
| Token file | `/var/run/secrets/cwii.dev/aws/token` |
| Token audience | `sts.amazonaws.com` (default) |
| `expirationSeconds` | `3600` (default; minimum `600`) |
| `AWS_ROLE_ARN` | from `cwii.dev/aws-role-arn` |
| `AWS_WEB_IDENTITY_TOKEN_FILE` | `/var/run/secrets/cwii.dev/aws/token` |
| `AWS_REGION` | from `cwii.dev/aws-region` (optional) |
| `AWS_ROLE_SESSION_NAME` | from `cwii.dev/aws-role-session-name` (optional) |

The webhook also records its work in the status annotation `cwii.dev/injected`,
a comma-joined sorted list of provider abbreviations (e.g. `aws`, or `aws,gcp`
if GCP is injected too).

!!! note "Each cloud gets its own token volume"
    cwii mounts a **separate** projected `serviceAccountToken` per enabled
    provider (`cwii-aws-token`, `cwii-gcp-token`, `cwii-az-token`), because each
    cloud's STS requires a **different** token audience. AWS uses
    `sts.amazonaws.com`; the volumes never collide.

---

## Step 4 — Verify

Set `cwii.dev/aws-verify: "true"` to have cwii add the **`cwii-aws-verify`** init
container, which runs:

```bash
aws sts get-caller-identity
```

using the image `amazon/aws-cli:latest` (override with `cwii.dev/aws-verify-image`
or the Helm value `providers.aws.verifyImage`).

```yaml
metadata:
  annotations:
    cwii.dev/aws-role-arn: "arn:aws:iam::123456789012:role/cwii-my-app"
    cwii.dev/aws-verify: "true"
```

| Mode | Behavior |
| --- | --- |
| `cwii.dev/aws-verify: "true"` | **Non-blocking.** The check is wrapped as `<check> \|\| echo … >&2`, so it always exits 0 — failures are logged only, the pod still starts. |
| `cwii.dev/aws-verify: "true"` + `cwii.dev/aws-verify-enforce: "true"` | **Blocking.** The check runs bare; a non-zero exit fails the init container and blocks pod startup. |

Check the verify output:

```bash
kubectl logs aws-demo -c cwii-aws-verify
```

A successful run prints the assumed-role identity:

```json
{
    "UserId": "AROAEXAMPLEID:aws-demo",
    "Account": "123456789012",
    "Arn": "arn:aws:sts::123456789012:assumed-role/cwii-my-app/aws-demo"
}
```

See [Verification](./verification.md) for the full verify model across providers.

You can also confirm from inside the application container at runtime:

```bash
kubectl exec aws-demo -c app -- aws sts get-caller-identity
```

---

## Gotchas

!!! warning "Common failures"
    - **`sub` must be exact.** The token subject is
      `system:serviceaccount:<NAMESPACE>:<SERVICEACCOUNT>`. A mismatched
      namespace or ServiceAccount name fails the trust-policy condition. Use
      `StringLike` with `:*` only when you intentionally want a whole namespace.
    - **Condition key prefix must match the provider URL exactly.** The
      `:aud` / `:sub` condition keys are prefixed with the issuer **host + path
      without the scheme** (e.g. `oidc.example.com/my-cluster:sub`). If this
      doesn't match the registered provider URL, the conditions silently never
      match and STS denies the assume-role.
    - **`aud` must equal a client ID on the provider.** cwii's AWS token uses
      audience `sts.amazonaws.com`. That value must appear in the provider's
      `--client-id-list` **and** in the trust-policy `:aud` condition. If you
      override `cwii.dev/aws-audience`, update both.
    - **Issuer must be byte-identical everywhere.** kube-apiserver
      `--service-account-issuer`, the provider `--url`, and the condition-key
      prefix must all agree (modulo the scheme on the condition prefix).
    - **`InvalidIdentityToken` from STS** almost always means AWS could not
      fetch your public JWKS, or token clock skew. Re-check the
      [OIDC discovery document](./self-hosted-oidc.md) is publicly reachable.

---

## Related

- [Self-hosted OIDC setup](./self-hosted-oidc.md) — publish the cluster issuer and JWKS (the hard prerequisite).
- [Install](./install.md) — deploy the cwii webhook.
- [Annotations reference](./annotations.md) — every `cwii.dev/*` annotation and the precedence rules.
- [Verification](./verification.md) — the can-i / verify init-container model.
