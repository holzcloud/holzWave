{{/* The name of the chart, and the name of every object of a release. */}}
{{- define "holzwave.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "holzwave.fullname" -}}
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

{{- define "holzwave.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{ include "holzwave.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "holzwave.selectorLabels" -}}
app.kubernetes.io/name: {{ include "holzwave.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "holzwave.serviceAccountName" -}}
{{- default (include "holzwave.fullname" .) .Values.serviceAccount.name -}}
{{- end -}}

{{/* The image: the digest is preferred over the tag, and `latest` is never a tag. */}}
{{- define "holzwave.image" -}}
{{- if .Values.image.digest -}}
{{- printf "%s@%s" .Values.image.repository .Values.image.digest -}}
{{- else -}}
{{- $tag := default .Chart.AppVersion .Values.image.tag -}}
{{- if eq $tag "latest" -}}
{{- fail "image.tag must not be latest: name a version or a digest" -}}
{{- end -}}
{{- printf "%s:%s" .Values.image.repository $tag -}}
{{- end -}}
{{- end -}}

{{/* The TLS mode, checked. */}}
{{- define "holzwave.mode" -}}
{{- $mode := dig "tls" "mode" "" .Values.config -}}
{{- if not (has $mode (list "files" "acme" "proxy")) -}}
{{- fail "config.tls.mode must be files, acme or proxy" -}}
{{- end -}}
{{- $mode -}}
{{- end -}}

{{/*
The configuration file of the server: the operator's `config` with the keys that only the chart knows forced over it (the data
and runtime directories, the listen address, the paths of the mounted TLS files and of the single sign-on client secrets).
*/}}
{{- define "holzwave.config" -}}
{{- $mode := include "holzwave.mode" . -}}
{{- $c := deepCopy .Values.config -}}
{{- $tls := deepCopy (get $c "tls") -}}
{{- if eq $mode "files" -}}
{{- if not .Values.tls.existingSecret -}}
{{- fail "tls.existingSecret is required in the files mode: the name of a Secret with the keys tls.crt and tls.key" -}}
{{- end -}}
{{- $_ := set $tls "cert_file" "/etc/holzwave/tls/tls.crt" -}}
{{- $_ := set $tls "key_file" "/etc/holzwave/tls/tls.key" -}}
{{- end -}}
{{- if eq $mode "proxy" -}}
{{- if not (get $tls "trusted_proxies") -}}
{{- fail "config.tls.trusted_proxies is required in the proxy mode: name the addresses or networks of the proxies that may connect (the list is empty)" -}}
{{- end -}}
{{- end -}}
{{- $_ := set $c "tls" $tls -}}
{{- $_ := set $c "data_dir" "/data" -}}
{{- $_ := set $c "runtime_dir" "/run/holzwave" -}}
{{- $_ := set $c "listen" "0.0.0.0:8443" -}}
{{- $auth := get $c "auth" -}}
{{- if $auth -}}
{{- range $provider := (get $auth "oidc") -}}
{{- range $secret := $.Values.oidcSecrets -}}
{{- if eq $secret.id (get $provider "id") -}}
{{- $_ := set $provider "client_secret_file" (printf "/etc/holzwave/oidc/%s/secret" $secret.id) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- toPrettyJson $c -}}
{{- end -}}
