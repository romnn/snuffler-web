{{/*
The page's `config.js`, which the app reads before it starts.
web/config.js in the repository documents its fields.
*/}}
{{- define "snuffler-web.configJs" -}}
{{- $config := dict -}}
{{- with .Values.proxyUrl }}
{{- $_ := set $config "proxyUrl" . }}
{{- end -}}
{{- with .Values.analytics }}
{{- if .scriptUrl }}
{{- $analytics := dict "scriptUrl" .scriptUrl }}
{{- with .attributes }}
{{- $attributes := dict }}
{{- range $name, $value := . }}
{{- $_ := set $attributes $name (toString $value) }}
{{- end }}
{{- $_ := set $analytics "attributes" $attributes }}
{{- end }}
{{- $_ := set $config "analytics" $analytics }}
{{- end }}
{{- end -}}
// Written by the snuffler-web Helm chart.
globalThis.snufflerConfig = {{ toJson $config }};
{{- end }}
