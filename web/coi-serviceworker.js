/*! coi-serviceworker v0.1.7 - Guido Zuidhof and contributors, licensed under MIT */
let coepCredentialless = false;
if (typeof window === 'undefined') {
    self.addEventListener("install", () => self.skipWaiting());
    self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

    self.addEventListener("message", (ev) => {
        // A parent deployment's scope also covers nested deployments until they
        // acquire their own controller, so only accept its own page handshake.
        if (!ev.data || ev.data.deployment !== self.registration.scope) {
            return;
        } else if (ev.data.type === "deregister") {
            self.registration
                .unregister()
                .then(() => {
                    return self.clients.matchAll();
                })
                .then(clients => {
                    clients.forEach((client) => client.navigate(client.url));
                });
        } else if (ev.data.type === "coepCredentialless") {
            coepCredentialless = ev.data.value;
        }
    });

    self.addEventListener("fetch", function (event) {
        const r = event.request;
        if (r.cache === "only-if-cached" && r.mode !== "same-origin") {
            return;
        }

        const request = (coepCredentialless && r.mode === "no-cors")
            ? new Request(r, {
                credentials: "omit",
            })
            : r;
        event.respondWith(
            fetch(request)
                .then((response) => {
                    if (response.status === 0) {
                        return response;
                    }

                    const newHeaders = new Headers(response.headers);
                    newHeaders.set("Cross-Origin-Embedder-Policy",
                        coepCredentialless ? "credentialless" : "require-corp"
                    );
                    if (!coepCredentialless) {
                        newHeaders.set("Cross-Origin-Resource-Policy", "cross-origin");
                    }
                    newHeaders.set("Cross-Origin-Opener-Policy", "same-origin");

                    return new Response(response.body, {
                        status: response.status,
                        statusText: response.statusText,
                        headers: newHeaders,
                    });
                })
                .catch((e) => console.error(e))
        );
    });

} else {
    (() => {
        const scriptUrl = new URL(window.document.currentScript.src);
        const deployment = new URL(".", scriptUrl).href;
        const sessionPrefix = `snuffler:${new URL(deployment).pathname}:coi.`;
        // Refusing storage must not prevent isolation, including in private mode.
        const session = {
            get(name) {
                try { return window.sessionStorage.getItem(sessionPrefix + name); }
                catch { return null; }
            },
            set(name, value) {
                try { window.sessionStorage.setItem(sessionPrefix + name, value); }
                catch { /* Isolation can proceed without remembering the reload. */ }
            },
            remove(name) {
                try { window.sessionStorage.removeItem(sessionPrefix + name); }
                catch { /* Storage may be disabled independently of service workers. */ }
            },
        };
        const reloadedBySelf = session.get("reloadedBySelf");
        session.remove("reloadedBySelf");
        const coepDegrading = reloadedBySelf === "coepdegrade";

        // You can customize the behavior of this script through a global `coi` variable.
        const coi = {
            shouldRegister: () => !reloadedBySelf,
            shouldDeregister: () => false,
            coepCredentialless: () => true,
            coepDegrade: () => true,
            doReload: () => window.location.reload(),
            quiet: false,
            ...window.coi
        };
        const workers = navigator.serviceWorker;
        const ownController = () => {
            const controller = workers && workers.controller;
            return controller && controller.scriptURL === scriptUrl.href ? controller : null;
        };
        let reloading = false;
        function reload(reason) {
            if (reloading) return;
            reloading = true;
            session.set("reloadedBySelf", reason);
            coi.doReload(reason);
        }
        function configureController(diagnoseIsolation) {
            const controller = ownController();
            if (!controller) return;

            // Only this deployment's controller can diagnose its isolation failure.
            const isolationFailed = diagnoseIsolation && !window.crossOriginIsolated;
            if (isolationFailed) session.set("coepHasFailed", "true");
            const reloadToDegrade = isolationFailed && coi.coepDegrade() && !coepDegrading;
            controller.postMessage({
                type: "coepCredentialless",
                deployment,
                value: (reloadToDegrade || session.get("coepHasFailed") && coi.coepDegrade())
                    ? false
                    : coi.coepCredentialless(),
            });
            if (reloadToDegrade) {
                !coi.quiet && console.log("Reloading page to degrade COEP.");
                reload("coepdegrade");
            }
            if (coi.shouldDeregister()) {
                controller.postMessage({ type: "deregister", deployment });
            }
        }
        configureController(true);

        // An isolated page may still be controlled by its parent deployment.
        // It must acquire its own registration before participating in handshakes.
        if (window.crossOriginIsolated === undefined || !coi.shouldRegister()) return;
        if (window.crossOriginIsolated && !(workers && workers.controller)) return;
        if (!window.isSecureContext) {
            !coi.quiet && console.log("COOP/COEP Service Worker not registered, a secure context is required.");
            return;
        }
        // In some environments (e.g. Firefox private mode) this won't be available.
        if (!workers) {
            !coi.quiet && console.error("COOP/COEP Service Worker not registered, perhaps due to private mode.");
            return;
        }

        workers.addEventListener("controllerchange", () => {
            if (!ownController()) return;
            // Claiming a document cannot retroactively isolate its response.
            // Diagnose COEP only after navigation through this controller.
            configureController(false);
            // Activation, rather than updatefound, makes the new worker usable.
            !coi.quiet && console.log("Reloading page to use this deployment's COOP/COEP Service Worker.");
            reload("controllerchange");
        });
        workers.register(scriptUrl.href, { scope: deployment }).then(
            (registration) => {
                !coi.quiet && console.log("COOP/COEP Service Worker registered", registration.scope);
                // An already-active nested registration needs navigation to replace
                // a parent controller; clients.claim() only runs at activation.
                if (registration.active && !ownController()) reload("notcontrolling");
            },
            (err) => {
                !coi.quiet && console.error("COOP/COEP Service Worker failed to register:", err);
            }
        );
    })();
}
