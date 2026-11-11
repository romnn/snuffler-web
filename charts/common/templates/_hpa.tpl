{{- define "common.hpa" -}}
{{- if .Values.autoscaling.enabled -}}
{{- $metrics := list -}}
{{- with .Values.autoscaling.targetCPUUtilizationPercentage }}
{{- $metrics = append $metrics (dict "type" "Resource" "resource" (dict "name" "cpu" "target" (dict "type" "Utilization" "averageUtilization" .))) }}
{{- end }}
{{- with .Values.autoscaling.targetMemoryUtilizationPercentage }}
{{- $metrics = append $metrics (dict "type" "Resource" "resource" (dict "name" "memory" "target" (dict "type" "Utilization" "averageUtilization" .))) }}
{{- end }}
{{- $metrics = concat $metrics (.Values.autoscaling.metrics | default list) -}}
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: {{ include "common.fullname" . }}
  labels:
    {{- include "common.labels" . | nindent 4 }}
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: {{ include "common.fullname" . }}
  minReplicas: {{ .Values.autoscaling.minReplicas }}
  maxReplicas: {{ .Values.autoscaling.maxReplicas }}
  {{- with $metrics }}
  metrics:
    {{- toYaml . | nindent 4 }}
  {{- end }}
  {{- with .Values.autoscaling.behavior }}
  behavior:
    {{- toYaml . | nindent 4 }}
  {{- end }}
{{- end }}
{{- end }}
