//! The hosts snuffler-web talks to, which the proxy always allows.

/// The FDSN data centers the app knows by name.
///
/// Keep in sync with `_SERVICE_BASE_URLS` in `web/snuffler_bridge/fdsn.py`.
const FDSN_HOSTS: &[&str] = &[
    "geofon.gfz.de",
    "service.earthscope.org",
    "service.iris.edu",
    "earthquake.usgs.gov",
    "www.orfeus-eu.org",
    "webservices.ingv.it",
    "ws.resif.fr",
    "eida.bgr.de",
];

/// The FDSN query endpoints the app calls on each data center.
const FDSN_QUERY_PATHS: &[&str] = &[
    "/fdsnws/dataselect/1/query",
    "/fdsnws/event/1/query",
    "/fdsnws/station/1/query",
];

/// The app's default live `SeedLink` server, `EarthScope`'s ring server, plus GEOFON's.
pub(crate) const SEEDLINK_SERVERS: &[&str] =
    &["rtserve.earthscope.org:18000", "geofon.gfz.de:18000"];

/// Every FDSN query URL the app may request through `/cors`.
pub(crate) fn fdsn_query_urls() -> impl Iterator<Item = String> {
    FDSN_HOSTS.iter().flat_map(|host| {
        FDSN_QUERY_PATHS
            .iter()
            .map(move |path| format!("https://{host}{path}"))
    })
}
