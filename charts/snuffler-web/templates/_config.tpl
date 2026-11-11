{{/*
The page's `config.js`, which the app reads before it starts.
web/config.js in the repository documents its fields.
*/}}
{{- define "snuffler-web.configJs" -}}
{{- $config := dict -}}
{{- with .Values.proxyUrl }}
{{- $_ := set $config "proxyUrl" . }}
{{- end -}}
// Written by the snuffler-web Helm chart.
globalThis.snufflerConfig = {{ toJson $config }};
{{- end }}
