use anyhow::Context;
use opentelemetry::global;
use opentelemetry::trace::TracerProvider;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, MetricExporter};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use std::sync::OnceLock;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

fn get_resource() -> Resource {
    static RESOURCE: OnceLock<Resource> = OnceLock::new();
    RESOURCE
        .get_or_init(|| {
            Resource::builder()
                .with_service_name("gorules-agent")
                .build()
        })
        .clone()
}

fn tracer() -> anyhow::Result<SdkTracerProvider> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()
        .context("Failed to create span exporter")?;

    Ok(SdkTracerProvider::builder()
        .with_resource(get_resource())
        .with_batch_exporter(exporter)
        .build())
}

fn logger() -> anyhow::Result<SdkLoggerProvider> {
    let exporter = LogExporter::builder()
        .with_tonic()
        .build()
        .context("Failed to create log exporter")?;

    Ok(SdkLoggerProvider::builder()
        .with_resource(get_resource())
        .with_batch_exporter(exporter)
        .build())
}

fn metrics() -> anyhow::Result<SdkMeterProvider> {
    let exporter = MetricExporter::builder()
        .with_tonic()
        .build()
        .expect("Failed to create metric exporter");

    Ok(SdkMeterProvider::builder()
        .with_periodic_exporter(exporter)
        .with_resource(get_resource())
        .build())
}

fn default_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

pub fn setup(otlp_enabled: bool) -> anyhow::Result<()> {
    let fmt_only = !otlp_enabled;

    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
        .fmt_fields(tracing_subscriber::fmt::format::JsonFields::new())
        .with_file(true)
        .with_line_number(true)
        .with_level(true)
        .with_current_span(fmt_only)
        .with_span_list(false)
        .with_target(false)
        .with_filter(default_filter());
    if !otlp_enabled {
        tracing_subscriber::registry().with(fmt_layer).init();
        return Ok(());
    }

    let logger_provider = logger()?;
    let metrics_provider = metrics()?;
    let tracer_provider = tracer()?;

    global::set_meter_provider(metrics_provider.clone());
    global::set_tracer_provider(tracer_provider.clone());

    let logger_layer =
        OpenTelemetryTracingBridge::new(&logger_provider).with_filter(default_filter());
    let tracer_layer =
        tracing_opentelemetry::OpenTelemetryLayer::new(tracer_provider.tracer("main"))
            .with_filter(default_filter());
    let metrics_layer =
        tracing_opentelemetry::MetricsLayer::new(metrics_provider).with_filter(default_filter());

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(logger_layer)
        .with(tracer_layer)
        .with(metrics_layer)
        .init();

    Ok(())
}
