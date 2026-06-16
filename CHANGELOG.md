# Changelog

All notable changes to cwii are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com), this project adheres to
[Semantic Versioning](https://semver.org), and releases are automated by
[release-please](https://github.com/googleapis/release-please) from Conventional Commits.

## [0.1.1](https://github.com/cluster-workload-identity/cwii/compare/v0.1.0...v0.1.1) (2026-06-16)


### Features

* Implement cwii core ([086ff26](https://github.com/cluster-workload-identity/cwii/commit/086ff261032713267513829f2df0016622f84042))
* Implement cwii core ([f5e6b9c](https://github.com/cluster-workload-identity/cwii/commit/f5e6b9c1257e8e1c68232483a0047e24021a9160))


### Bug Fixes

* 1.33 ([9e77930](https://github.com/cluster-workload-identity/cwii/commit/9e779308d1845e50a3c3c0a60df3d4003e62bcdf))
* bump dependencies ([cd0a03c](https://github.com/cluster-workload-identity/cwii/commit/cd0a03c802174f7c7aa516ac6cbfa7ee6d2117b9))
* bump dependencies ([41ecae2](https://github.com/cluster-workload-identity/cwii/commit/41ecae2c044608e064e5607a86a68b0e6221629d))
* Credits in readme and taplo ([f986470](https://github.com/cluster-workload-identity/cwii/commit/f986470faf303e585976e06ad5a6d35e569f7870))
* glibc error due to mismatching debian versions ([ae77afc](https://github.com/cluster-workload-identity/cwii/commit/ae77afc867e48285608e457195c91cd5a38c317d))
* Review fixes ([c4f8ca3](https://github.com/cluster-workload-identity/cwii/commit/c4f8ca390c26095f29a76ca039bdb6aa559fcdce))

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
