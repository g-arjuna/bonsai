{{/*
Expand the name of the chart.
*/}}
{{- define "bonsai.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "bonsai.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Create chart label.
*/}}
{{- define "bonsai.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels.
*/}}
{{- define "bonsai.labels" -}}
helm.sh/chart: {{ include "bonsai.chart" . }}
{{ include "bonsai.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels.
*/}}
{{- define "bonsai.selectorLabels" -}}
app.kubernetes.io/name: {{ include "bonsai.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
ServiceAccount name.
*/}}
{{- define "bonsai.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "bonsai.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Secret name — prefer existingSecret, else use chart-generated name.
*/}}
{{- define "bonsai.secretName" -}}
{{- if .Values.secrets.existingSecret }}
{{- .Values.secrets.existingSecret }}
{{- else }}
{{- printf "%s-credentials" (include "bonsai.fullname" .) }}
{{- end }}
{{- end }}

{{/*
PVC name for archive storage.
*/}}
{{- define "bonsai.archivePvcName" -}}
{{- printf "%s-archive" (include "bonsai.fullname" .) }}
{{- end }}

{{/*
PVC name for graph database storage.
*/}}
{{- define "bonsai.graphPvcName" -}}
{{- printf "%s-graph" (include "bonsai.fullname" .) }}
{{- end }}

{{/*
Image reference.
*/}}
{{- define "bonsai.image" -}}
{{- printf "%s:%s" .Values.image.repository .Values.image.tag }}
{{- end }}

{{/*
Sidecar image reference.
*/}}
{{- define "bonsai.sidecarImage" -}}
{{- printf "%s:%s" .Values.sidecar.image.repository .Values.sidecar.image.tag }}
{{- end }}
