# Changelog

All notable changes to cwii are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com), this project adheres to
[Semantic Versioning](https://semver.org), and releases are automated by
[release-please](https://github.com/googleapis/release-please) from Conventional Commits.

## 0.1.0

Initial release.

- Multi-cloud workload identity federation injection for **GCP**, **AWS** and **Azure**.
- Per-provider projected ServiceAccount tokens (separate audiences) mounted under
  `/var/run/secrets/cwii.dev/<provider>/`.
- GCP credentials delivery via ConfigMap or init container; direct federation or service-account
  impersonation.
- Opt-in "can-i" verification init containers per provider.
- Industry-standard Helm chart with cert-manager or self-signed TLS, conditional RBAC, and a
  deadlock-safe `failurePolicy: Fail` option.
- Multi-arch distroless image, cosign-signed with SBOM + provenance; OCI Helm chart.
