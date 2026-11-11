{{- define "common.pdb" -}}
{{- if .Values.podDisruptionBudget.enabled -}}
apiVersion: policy/v1
kind: PodDisruptionBudget
metadata:
  name: {{ include "common.fullname" . }}
  labels:
    {{- include "common.labels" . | nindent 4 }}
spec:
  {{- /*
  Kubernetes takes one of the two budgets, so a set `minAvailable` wins.
  Zero is a meaningful budget, so only an unset value counts as unset.
  */}}
  {{- if not (kindIs "invalid" .Values.podDisruptionBudget.minAvailable) }}
  minAvailable: {{ .Values.podDisruptionBudget.minAvailable }}
  {{- else if not (kindIs "invalid" .Values.podDisruptionBudget.maxUnavailable) }}
  maxUnavailable: {{ .Values.podDisruptionBudget.maxUnavailable }}
  {{- end }}
  {{- with .Values.podDisruptionBudget.unhealthyPodEvictionPolicy }}
  unhealthyPodEvictionPolicy: {{ . }}
  {{- end }}
  selector:
    matchLabels:
      {{- include "common.selectorLabels" . | nindent 6 }}
{{- end }}
{{- end }}
