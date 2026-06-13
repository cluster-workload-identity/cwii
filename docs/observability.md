# Observability

cwii emits structured logs always, and — when enabled — OpenTelemetry **traces** and **metrics**
over OTLP. The OTel-native model is to push to an [OpenTelemetry Collector](https://opentelemetry.io/docs/collector/),
which then fans out to your backends (Prometheus, Tempo, Jaeger, Grafana, etc.).

## Enabling

Helm:

```yaml
otel:
  enabled: true
  endpoint: "http://otel-collector.observability:4317" # OTLP/gRPC
```

Or directly:

```bash
cwii --otel-enabled=true --otel-endpoint=http://otel-collector:4317
# or rely on the standard OTLP env var:
cwii --otel-enabled=true   # uses OTEL_EXPORTER_OTLP_ENDPOINT
```

When `otel.enabled=false` (the default), the OTel pipeline is never built and recording is a no-op —
only structured logs are emitted (`RUST_LOG` / the `logLevel` value controls verbosity).

Export targets honour the standard `OTEL_EXPORTER_OTLP_*` environment variables; `--otel-endpoint`
is a convenience override. The service name is reported as `cwii`.

## Metrics

| Metric | Type | Attributes | Meaning |
| --- | --- | --- | --- |
| `cwii.admission.requests` | counter | `outcome` = `skip` \| `inject` \| `deny` \| `patch_error` \| `bad_request` | AdmissionReview requests handled |
| `cwii.injections` | counter | `provider` = `gcp` \| `aws` \| `az` | Provider injections planned |
| `cwii.admission.duration` | histogram (s) | — | Time spent handling a mutation |

Useful starting points: alert on a rising `deny`/`patch_error` rate, watch p99 of
`cwii.admission.duration` against the webhook `timeoutSeconds`, and track `cwii.injections` by
provider to see adoption.

## Traces

The webhook's `tracing` spans (`mutate`, `handle`, and the per-request events) are exported as OTel
spans via the `tracing-opentelemetry` bridge. Each mutation is one trace, so you can see annotation
resolution, the cluster reads, and (for GCP configMap mode) the ConfigMap upsert on a timeline.

## Collector example

A minimal Collector config that accepts cwii's OTLP and exposes Prometheus + sends traces onward:

```yaml
receivers:
  otlp:
    protocols:
      grpc:
exporters:
  prometheus:
    endpoint: 0.0.0.0:8889
  otlphttp/tempo:
    endpoint: http://tempo:4318
service:
  pipelines:
    metrics: { receivers: [otlp], exporters: [prometheus] }
    traces: { receivers: [otlp], exporters: [otlphttp/tempo] }
```

Because metrics are pushed via OTLP rather than scraped, the chart ships **no** `/metrics` endpoint
or `ServiceMonitor` — let the Collector own Prometheus exposure.
