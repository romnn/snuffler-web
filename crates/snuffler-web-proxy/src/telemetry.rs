//! Logs to stderr, and, when an OTLP endpoint is configured, also exports traces, metrics and
//! logs to an OpenTelemetry collector.
//!
//! The stderr log is human-readable by default and JSON for log shippers; see [`LogFormat`].
//!
//! Metrics are `tracing` events with `monotonic_counter.*`, `counter.*` or `histogram.*`
//! fields, which [`tracing_opentelemetry::MetricsLayer`] turns into OpenTelemetry instruments.
//! Every other field of such an event becomes an attribute of the metric, its message included,
//! so a metric event has no message and only fields with a handful of values.

use std::io::IsTerminal as _;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::filter::{FilterExt as _, filter_fn};
use tracing_subscriber::layer::Filter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer, Registry};

/// The `service.name` every exported span, metric and log carries.
const SERVICE_NAME: &str = "snuffler-web-proxy";

/// The log filter without `--log-filter`: this binary and the two proxies it runs.
const DEFAULT_FILTER: &str = "snuffler_web_proxy=info,cors_proxy=info,tcp_over_ws_proxy=info";

/// What the stderr log shows on top of [`DEFAULT_FILTER`]: the OpenTelemetry SDK's own
/// warnings, such as a collector that cannot be reached.
const SDK_WARNINGS: &str = "opentelemetry=warn";

/// Target prefixes of the crates the exporter itself runs on, which the export never sees,
/// whatever filter the operator sets.
///
/// Exporting a batch makes `tonic`, `h2`, `hyper` and `tower` emit events of their own, and the
/// SDK reports its failures as events too.
/// Exported, each of those would be exported in turn, a feedback loop that grows with every
/// batch.
/// A separate filter shuts them out rather than `=off` directives, which a more specific
/// directive such as `h2::codec=trace` would override.
const EXPORTER_STACK: [&str; 5] = ["opentelemetry", "tonic", "h2", "hyper", "tower"];

/// How the stderr log is written.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum LogFormat {
    /// One human-readable line per event, for a terminal.
    #[default]
    Pretty,
    /// One JSON object per event, for a log shipper.
    Json,
}

/// Where log events go besides stderr while an OTLP endpoint is set, under the names of
/// OpenTelemetry's `OTEL_LOGS_EXPORTER`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum)]
pub enum LogsExporter {
    /// Exported over OTLP, beside the traces and metrics.
    #[default]
    Otlp,
    /// Kept to stderr, for a deployment that already collects it, which would otherwise store
    /// every event twice.
    #[value(name = "none")]
    Disabled,
}

/// Why telemetry could not be set up.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    /// An OTLP exporter could not be built for the configured endpoint.
    #[error("failed to build the OTLP exporter for {endpoint}")]
    Exporter {
        /// The configured `--otel-endpoint`.
        endpoint: String,
        /// Why the exporter could not be built.
        #[source]
        source: opentelemetry_otlp::ExporterBuildError,
    },
    /// The `--log-filter` is not a valid filter.
    #[error("invalid log filter {filter:?}")]
    InvalidFilter {
        /// The rejected filter.
        filter: String,
        /// Why it did not parse.
        #[source]
        source: tracing_subscriber::filter::ParseError,
    },
}

/// Keeps the OpenTelemetry providers alive, and flushes them when dropped.
pub struct Telemetry {
    providers: Option<Providers>,
}

struct Providers {
    traces: SdkTracerProvider,
    metrics: SdkMeterProvider,
    /// Absent under [`LogsExporter::Disabled`].
    logs: Option<SdkLoggerProvider>,
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        let Some(providers) = self.providers.take() else {
            return;
        };
        // The subscriber is being torn down with these providers, so report on stderr.
        if let Err(err) = providers.traces.shutdown() {
            eprintln!("failed to flush traces: {err}");
        }
        if let Err(err) = providers.metrics.shutdown() {
            eprintln!("failed to flush metrics: {err}");
        }
        if let Some(logs) = providers.logs
            && let Err(err) = logs.shutdown()
        {
            eprintln!("failed to flush logs: {err}");
        }
    }
}

/// Installs the global `tracing` subscriber.
///
/// `log_filter` is an [`EnvFilter`] directive list, such as `debug` or
/// `info,cors_proxy=trace`, and replaces the default for both the stderr log and the export;
/// the export still never sees the [`EXPORTER_STACK`].
///
/// Without `otel_endpoint` only the stderr log is installed.
/// With one, spans, metrics and, unless `logs_exporter` is [`LogsExporter::Disabled`], log
/// events are also exported over OTLP gRPC, and the stderr log keeps going.
/// The exporters connect lazily, so an unreachable collector never blocks startup.
///
/// The W3C trace-context propagator becomes the [`opentelemetry::global`] one, which the
/// request spans read an incoming `traceparent` with.
/// Must run inside the tokio runtime, which the gRPC exporters use.
///
/// # Errors
///
/// Returns [`TelemetryError::InvalidFilter`] for a `log_filter` that does not parse, and
/// [`TelemetryError::Exporter`] when an exporter cannot be built for the endpoint.
pub fn init(
    log_format: LogFormat,
    log_filter: Option<&str>,
    otel_endpoint: Option<&str>,
    logs_exporter: LogsExporter,
) -> Result<Telemetry, TelemetryError> {
    let stderr_filter = match log_filter {
        Some(filter) => parse_filter(filter)?,
        None => parse_filter(&format!("{DEFAULT_FILTER},{SDK_WARNINGS}"))?,
    };
    let stderr = stderr_layer(log_format).with_filter(stderr_filter);

    let Some(endpoint) = otel_endpoint else {
        tracing_subscriber::registry().with(stderr).init();
        return Ok(Telemetry { providers: None });
    };
    let providers =
        build_providers(endpoint, logs_exporter).map_err(|source| TelemetryError::Exporter {
            endpoint: endpoint.to_owned(),
            source,
        })?;
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

    let traces = tracing_opentelemetry::layer()
        .with_tracer(providers.traces.tracer(SERVICE_NAME))
        .with_filter(export_filter(log_filter)?);
    let metrics = tracing_opentelemetry::MetricsLayer::new(providers.metrics.clone())
        .with_filter(export_filter(log_filter)?);
    let logs = match &providers.logs {
        Some(logs) => Some(
            opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(logs)
                .with_filter(export_filter(log_filter)?),
        ),
        None => None,
    };
    let exporting_logs = logs.is_some();
    tracing_subscriber::registry()
        .with(stderr)
        .with(traces)
        .with(metrics)
        .with(logs)
        .init();
    if exporting_logs {
        tracing::info!(endpoint, "exporting traces, metrics and logs over OTLP");
    } else {
        tracing::info!(endpoint, "exporting traces and metrics over OTLP");
    }

    Ok(Telemetry {
        providers: Some(providers),
    })
}

fn parse_filter(filter: &str) -> Result<EnvFilter, TelemetryError> {
    EnvFilter::builder()
        .parse(filter)
        .map_err(|source| TelemetryError::InvalidFilter {
            filter: filter.to_owned(),
            source,
        })
}

/// The operator's filter, or the default, minus the [`EXPORTER_STACK`].
fn export_filter<S>(log_filter: Option<&str>) -> Result<impl Filter<S> + use<S>, TelemetryError>
where
    S: tracing::Subscriber,
{
    let filter = parse_filter(log_filter.unwrap_or(DEFAULT_FILTER))?;
    Ok(filter.and(filter_fn(|metadata| !is_exporter_stack(metadata.target()))))
}

fn is_exporter_stack(target: &str) -> bool {
    EXPORTER_STACK
        .iter()
        .any(|prefix| target.starts_with(prefix))
}

fn stderr_layer(log_format: LogFormat) -> Box<dyn Layer<Registry> + Send + Sync> {
    let layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
    match log_format {
        // Colour codes only belong on a terminal, never in a file or a supervisor's log.
        LogFormat::Pretty if !std::io::stderr().is_terminal() => layer.with_ansi(false).boxed(),
        LogFormat::Pretty => layer.boxed(),
        LogFormat::Json => layer.json().boxed(),
    }
}

fn build_providers(
    endpoint: &str,
    logs_exporter: LogsExporter,
) -> Result<Providers, opentelemetry_otlp::ExporterBuildError> {
    let resource = Resource::builder().with_service_name(SERVICE_NAME).build();

    let span_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;
    let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;
    let logs = match logs_exporter {
        LogsExporter::Otlp => {
            let exporter = opentelemetry_otlp::LogExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .build()?;
            Some(
                SdkLoggerProvider::builder()
                    .with_batch_exporter(exporter)
                    .with_resource(resource.clone())
                    .build(),
            )
        }
        LogsExporter::Disabled => None,
    };

    Ok(Providers {
        traces: SdkTracerProvider::builder()
            .with_batch_exporter(span_exporter)
            .with_resource(resource.clone())
            .build(),
        metrics: SdkMeterProvider::builder()
            .with_periodic_exporter(metric_exporter)
            .with_resource(resource)
            .build(),
        logs,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use color_eyre::eyre::{self, bail};
    use tracing_subscriber::layer::Context;

    use super::*;

    /// Records the target of every event it is shown.
    struct Targets(Arc<Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber> Layer<S> for Targets {
        fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
            if let Ok(mut targets) = self.0.lock() {
                targets.push(event.metadata().target().to_owned());
            }
        }
    }

    /// The export never sees the exporter's own crates, even when the operator turns
    /// everything up or names one of them more specifically, and still sees the proxy.
    #[test]
    fn the_export_never_sees_the_exporter_stack() -> eyre::Result<()> {
        for operator_filter in ["trace", "trace,h2::codec=trace,tonic::transport=trace"] {
            let targets = Arc::new(Mutex::new(Vec::new()));
            let export =
                Targets(Arc::clone(&targets)).with_filter(export_filter(Some(operator_filter))?);
            let subscriber = tracing_subscriber::registry().with(export);
            tracing::subscriber::with_default(subscriber, || {
                tracing::trace!(target: "h2::codec", "frame");
                tracing::trace!(target: "tonic::transport", "request");
                tracing::warn!(target: "opentelemetry_sdk", "export failed");
                tracing::trace!(target: "cors_proxy", "request");
            });
            let Ok(targets) = targets.lock() else {
                bail!("the target list was poisoned");
            };
            assert_eq!(*targets, ["cors_proxy"], "with {operator_filter:?}");
        }
        Ok(())
    }

    #[test]
    fn rejects_a_filter_that_does_not_parse() {
        assert!(matches!(
            parse_filter("cors_proxy=loud"),
            Err(TelemetryError::InvalidFilter { .. })
        ));
    }
}
