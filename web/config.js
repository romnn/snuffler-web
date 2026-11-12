// The deployment's runtime configuration.
// index.html loads it before the app starts, and the Python worker imports it while it boots,
// so it sets a global rather than `window`, which a worker has none of.
// This one names no proxy and tracks nothing.
// A deployment supplies its own: the build writes one from SNUFFLER_PROXY_URL and the
// SNUFFLER_ANALYTICS_* variables (scripts/trunk-post-build.sh), which micromux and the GitHub
// Pages workflow set, the Helm chart mounts one from a ConfigMap, and docker-compose.yaml mounts
// one too.
//
// - proxyUrl: where snuffler-web-proxy is served, such as "https://proxy.snuffler.example/".
//   Live SeedLink streams go through it, and FDSN requests a data center refuses to a
//   browser, such as one without CORS headers, are retried through it.
//   A relative URL resolves against the page.
//   Unset, the app reaches only data centers that allow cross-origin requests, and cannot
//   stream live SeedLink data.
// - analytics: a web analytics script, such as Umami's or Plausible's, as
//   { scriptUrl, attributes }; web/analytics.js adds it to the page.
//   - scriptUrl: the script, such as "https://stats.example/script.js".
//   - attributes: optional; the attributes the script reads its settings from, such as
//     { "data-website-id": "…", "data-domains": "snuffler.example" } for Umami.
//   Tell the script to leave out the query string where it can, such as with Umami's
//   "data-exclude-search": "true": a `?url=` deep link can carry a signed download URL, and
//   picking an example only replaces the query, which would count as another page view.
//   The script is fetched in CORS mode, so its host must answer with CORS headers.
//   Unset, nothing is tracked.
globalThis.snufflerConfig = {};
