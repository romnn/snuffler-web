{{/* Isolates the pods, then allows exactly the `ingress` and `egress` rules the values list. */}}
{{- define "common.networkPolicy" -}}
{{- if .Values.networkPolicy.enabled -}}
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: {{ include "common.fullname" . }}
  labels:
    {{- include "common.labels" . | nindent 4 }}
spec:
  podSelector:
    matchLabels:
      {{- include "common.selectorLabels" . | nindent 6 }}
  policyTypes:
    - Ingress
    - Egress
  ingress:
    {{- toYaml (.Values.networkPolicy.ingress | default list) | nindent 4 }}
  egress:
    {{- toYaml (.Values.networkPolicy.egress | default list) | nindent 4 }}
{{- end }}
{{- end }}
