// The deployment's runtime configuration.
// index.html loads it before the app starts, and the Python worker imports it while it boots,
// so it sets a global rather than `window`, which a worker has none of.
// This one names no proxy.
// A deployment supplies its own: the build writes one from SNUFFLER_PROXY_URL
// (scripts/trunk-post-build.sh), which micromux and the GitHub Pages workflow set, the Helm
// chart mounts one from a ConfigMap, and docker-compose.yaml mounts one too.
//
// - proxyUrl: where snuffler-web-proxy is served, such as "https://proxy.snuffler.example/".
//   Live SeedLink streams go through it, and FDSN requests a data center refuses to a
//   browser, such as one without CORS headers, are retried through it.
//   A relative URL resolves against the page.
//   Unset, the app reaches only data centers that allow cross-origin requests, and cannot
//   stream live SeedLink data.
globalThis.snufflerConfig = {};
