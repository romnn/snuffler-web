// Loads the web analytics script that the deployment's config.js names, and nothing without one
// (web/config.js documents `analytics`).
// index.html runs it right after config.js, so the script loads while the app boots.
(function () {
  "use strict";

  var analytics = globalThis.snufflerConfig && globalThis.snufflerConfig.analytics;
  if (!analytics || !analytics.scriptUrl) {
    return;
  }

  // Count only an isolated page, which the app needs to run at all.
  // On GitHub Pages a first visit is not isolated yet: coi-serviceworker.js reloads it once
  // its worker controls the page, and counting both loads would count the visit twice.
  if (!window.crossOriginIsolated) {
    return;
  }

  var script = document.createElement("script");
  script.src = analytics.scriptUrl;
  // A CORS request, so the script also loads where the service worker isolates the page with
  // COEP `require-corp` rather than `credentialless`; analytics hosts answer CORS, since their
  // scripts post across origins anyway.
  // An attribute below may override it.
  script.crossOrigin = "anonymous";
  var attributes = analytics.attributes || {};
  Object.keys(attributes).forEach(function (name) {
    script.setAttribute(name, attributes[name]);
  });
  document.head.appendChild(script);
})();
