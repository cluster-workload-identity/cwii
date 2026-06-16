{{/* Chart name, optionally overridden. */}}
{{- define "cwii.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/* Fully qualified app name. */}}
{{- define "cwii.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "cwii.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "cwii.labels" -}}
helm.sh/chart: {{ include "cwii.chart" . }}
{{ include "cwii.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: cwii
{{- end -}}

{{- define "cwii.selectorLabels" -}}
app.kubernetes.io/name: {{ include "cwii.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "cwii.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "cwii.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{/* Name of the TLS secret holding the webhook serving certificate. */}}
{{- define "cwii.certName" -}}
{{- printf "%s-tls" (include "cwii.fullname" .) -}}
{{- end -}}

{{/* Fully resolved container image reference. */}}
{{- define "cwii.image" -}}
{{- $tag := default .Chart.AppVersion .Values.image.tag -}}
{{- printf "%s:%s" .Values.image.repository $tag -}}
{{- end -}}

{{/* "true" when GCP ConfigMap delivery is active (the only mode needing ConfigMap-write RBAC). */}}
{{- define "cwii.gcpConfigMapMode" -}}
{{- if and .Values.providers.gcp.enabled (eq .Values.providers.gcp.deliveryMode "configMap") -}}true{{- else -}}false{{- end -}}
{{- end -}}

{{/*
namespaceSelector body shared by the webhook. Always excludes the release namespace plus
kube-system and kube-node-lease so a `failurePolicy: Fail` webhook can never deadlock its own
namespace or the control plane. Additional exclusions and an opt-in matchLabels are appended.
*/}}
{{- define "cwii.namespaceSelector" -}}
{{- $excluded := concat (list .Release.Namespace "kube-system" "kube-node-lease") .Values.webhook.namespaceSelector.excludeNamespaces | uniq -}}
matchExpressions:
  - key: kubernetes.io/metadata.name
    operator: NotIn
    values:
    {{- range $excluded }}
      - {{ . }}
    {{- end }}
{{- with .Values.webhook.namespaceSelector.matchLabels }}
matchLabels:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- end -}}

{{/*
The single mutating webhook entry. Rendered by both the cert-manager and self-signed templates.
Pass a dict: {ctx: $, caBundle: <base64 PEM or empty string>}. When caBundle is empty the field is
omitted (the cert-manager CA injector populates it instead).
*/}}
{{- define "cwii.webhooks" -}}
{{- $ctx := .ctx -}}
- name: mutate.cwii.dev
  admissionReviewVersions: ["v1"]
  sideEffects: {{ $ctx.Values.webhook.sideEffects }}
  failurePolicy: {{ $ctx.Values.webhook.failurePolicy }}
  reinvocationPolicy: {{ $ctx.Values.webhook.reinvocationPolicy }}
  matchPolicy: {{ $ctx.Values.webhook.matchPolicy }}
  timeoutSeconds: {{ $ctx.Values.webhook.timeoutSeconds }}
  clientConfig:
    service:
      name: {{ include "cwii.fullname" $ctx }}
      namespace: {{ $ctx.Release.Namespace }}
      path: /mutate
      port: {{ $ctx.Values.service.port }}
    {{- if .caBundle }}
    caBundle: {{ .caBundle }}
    {{- end }}
  rules:
    - apiGroups: [""]
      apiVersions: ["v1"]
      resources: ["pods"]
      operations: ["CREATE"]
      scope: Namespaced
  namespaceSelector:
    {{- include "cwii.namespaceSelector" $ctx | nindent 4 }}
  {{- with $ctx.Values.webhook.objectSelector }}
  objectSelector:
    {{- toYaml . | nindent 4 }}
  {{- end }}
  {{- with $ctx.Values.webhook.matchConditions }}
  matchConditions:
    {{- toYaml . | nindent 4 }}
  {{- end }}
{{- end -}}
