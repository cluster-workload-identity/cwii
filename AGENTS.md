# AGENTS.md

Guidance for AI coding agents (and humans) working in this repository. See
[CONTRIBUTING.md](./CONTRIBUTING.md) for the full contributor guide and [cwii.dev](https://cwii.dev)
for product docs.

## What this is

cwii is a multi-cloud Kubernetes **mutating admission webhook** (Rust) that injects workload
identity federation plumbing (GCP, AWS, Azure) into pods. The defining idea: each enabled provider
gets its **own** projected ServiceAccount token (different clouds need different token audiences).

## Repository layout

```
crates/
  cwii-core/          # provider-agnostic engine: Provider trait, plan IR, resolve, patch, admission, k8s
  cwii-provider-gcp/  # GCP external_account credentials.json + configMap/initContainer delivery
  cwii-provider-aws/  # AWS AssumeRoleWithWebIdentity (env vars only)
  cwii-provider-az/   # Azure Entra ID federation (env vars only)
  cwii/               # the binary: clap config, axum/rustls server, provider registry wiring
charts/cwii/          # Helm chart (industry-standard; cert-manager or self-signed TLS)
docs/                 # mkdocs-material site published to cwii.dev
.github/workflows/    # ci, release, release-please, docs
```

Data flow: `crates/cwii/src/main.rs` → `webhook.rs` (`AppState: WebhookState`) →
`cwii_core::mutate` → resolve annotations → each `Provider::plan()` → `plan::merge` →
`patch::build` (RFC 6902). Providers emit a declarative `ProviderPlan`; only `cwii-core::patch`
knows JSON-pointer syntax.

## Build, test, lint

The CI gate is: nightly `fmt --check`, `clippy -D warnings`, `cargo test`, `cargo-deny`,
`helm lint` + `kubeconform`, hadolint, yamllint, markdownlint, actionlint, and a kind e2e smoke test.
Run locally before pushing:

```bash
cargo build --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo +nightly fmt --all            # rustfmt.toml uses unstable features → nightly required
cargo deny check
helm lint charts/cwii
```

### Sandboxed environments (important)

If you run inside a sandbox that makes `~/.cargo` read-only (cargo fails to unpack crates with
`Operation not permitted`), point `CARGO_HOME` at a writable directory **outside the repo tree**:

```bash
export CARGO_HOME="$TMPDIR/cwii-cargo"
```

Keep it outside the repo: some crates (e.g. `fluent-uri`) ship a `.gitmodules` in their tarball, and
git-metadata writes inside the project tree are commonly blocked. The `.cargo-home/` name is
gitignored for this purpose.

## Conventions

- **Annotation keys are API.** Keys live in `crates/cwii-core/src/annotations.rs` (shared) and as
  provider-local consts. Changing one is a breaking change — update [docs/annotations.md](./docs/annotations.md).
- **Conventional Commits.** `feat:`/`fix:`/`feat!:` drive release-please's SemVer bump and CHANGELOG.
- **Match surrounding style.** rustfmt config is strict (`group_imports`, `imports_granularity=Module`,
  `wrap_comments`, `format_strings`). Just run `cargo +nightly fmt`.
- Chart value changes require a matching update to [docs/install.md](./docs/install.md) and `values.schema.json`.

## Adding a new provider

1. New crate `crates/cwii-provider-<abbr>/` depending on `cwii-core`; implement `Provider`
   (`id()` returns a `ProviderId`, `plan()` returns a `ProviderPlan`). Mirror `cwii-provider-aws`
   (env-only) or `cwii-provider-gcp` (file delivery).
2. Add a `ProviderId` variant in `crates/cwii-core/src/provider.rs` and its `abbr()`.
3. Register it in `crates/cwii/src/config.rs` (flags + `providers()`), add it to the root
   workspace deps and the binary deps.
4. Add Helm `providers.<abbr>` values + deployment flags, and a `docs/<abbr>-setup.md`.
5. Add unit tests (skip-without-required-config, token audience) and extend
   `crates/cwii/tests/multi_provider.rs`.

## Don'ts

- Don't hand-roll JSON patches in providers — return a `ProviderPlan`.
- Don't grant ConfigMap-write RBAC unless GCP `configMap` delivery is active.
- Don't weaken the webhook `namespaceSelector` exclusions (release ns + kube-system + kube-node-lease)
  — they prevent a `failurePolicy: Fail` deadlock.
