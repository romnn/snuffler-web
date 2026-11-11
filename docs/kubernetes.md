# Deploying on Kubernetes

Two Helm charts deploy snuffler-web:

- `snuffler-web` serves the app.
- `snuffler-web-proxy` lets it reach FDSN data centers that send no CORS headers, and stream
  live SeedLink data.

The browser talks to both directly, so each needs its own public address.
The examples use `snuffler.example` and `proxy.snuffler.example`.

## Install

The charts are published as OCI artifacts at `oci://ghcr.io/romnn/snuffler-web/charts`, one
version per release, which Helm 3.8 and later install directly.

`proxy.yaml`:

```yaml
ingress:
  enabled: true
  className: nginx
  annotations:
    # A live SeedLink stream holds its WebSocket open far longer than the default 60 s.
    nginx.ingress.kubernetes.io/proxy-read-timeout: "3600"
    nginx.ingress.kubernetes.io/proxy-send-timeout: "3600"
  hosts:
    - host: proxy.snuffler.example
      paths:
        - path: /
          pathType: Prefix
  tls:
    - secretName: proxy-snuffler-example-tls
      hosts:
        - proxy.snuffler.example
```

`web.yaml`:

```yaml
proxyUrl: https://proxy.snuffler.example/
ingress:
  enabled: true
  className: nginx
  hosts:
    - host: snuffler.example
      paths:
        - path: /
          pathType: Prefix
  tls:
    - secretName: snuffler-example-tls
      hosts:
        - snuffler.example
```

```sh
charts=oci://ghcr.io/romnn/snuffler-web/charts
helm install snuffler-web-proxy $charts/snuffler-web-proxy -f proxy.yaml
helm install snuffler-web $charts/snuffler-web -f web.yaml
helm test snuffler-web-proxy
helm test snuffler-web
```

## Configure

- **Resources:** the charts set no requests or limits; set `resources` for your cluster.
- **More data centers or SeedLink servers:** list them in the proxy's `allowHttpTargets` and
  `allowTcpTargets`.
- **Logs and telemetry:** the proxy logs JSON; `log.filter` takes a `RUST_LOG`-style filter,
  and `otel.endpoint` exports traces, metrics and logs to an OpenTelemetry collector.
- **Everything else**, such as autoscaling, disruption budgets, network policies and pod
  placement: `helm show values oci://ghcr.io/romnn/snuffler-web/charts/snuffler-web` lists each value with its
  description, and the charts reject a value they do not know.
