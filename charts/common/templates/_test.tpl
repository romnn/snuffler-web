{{/*
A `helm test` pod that asks the Service for the app's health endpoint.
It leaves out `app.kubernetes.io/name`, so the Service, the PDB and the NetworkPolicy, which select
the app's pods by name and instance, never select it.
*/}}
{{- define "common.test" -}}
{{- if .Values.tests.enabled -}}
apiVersion: v1
kind: Pod
metadata:
  name: {{ include "common.fullname" . }}-test
  labels:
    helm.sh/chart: {{ include "common.chart" . }}
    app.kubernetes.io/instance: {{ .Release.Name }}
    app.kubernetes.io/component: test
    app.kubernetes.io/managed-by: {{ .Release.Service }}
    app.kubernetes.io/part-of: snuffler-web
    {{- with .Values.commonLabels }}
    {{- toYaml . | nindent 4 }}
    {{- end }}
  annotations:
    helm.sh/hook: test
    helm.sh/hook-delete-policy: before-hook-creation,hook-succeeded
spec:
  restartPolicy: Never
  automountServiceAccountToken: false
  securityContext:
    runAsNonRoot: true
    runAsUser: 65534
    runAsGroup: 65534
    seccompProfile:
      type: RuntimeDefault
  containers:
    - name: wget
      image: {{ .Values.tests.image | quote }}
      command:
        - wget
        - -q
        - -O
        - /dev/null
        - http://{{ include "common.fullname" . }}:{{ .Values.service.port }}/health
      securityContext:
        allowPrivilegeEscalation: false
        readOnlyRootFilesystem: true
        capabilities:
          drop:
            - ALL
      {{- with .Values.tests.resources }}
      resources:
        {{- toYaml . | nindent 8 }}
      {{- end }}
{{- end }}
{{- end }}
