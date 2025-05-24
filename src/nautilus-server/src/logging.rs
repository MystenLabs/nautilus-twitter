// Copyright (c), Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use reqwest::Client;
use reqwest::Response;
use serde_json::{json, Value};
use std::collections::HashMap;
use telemetry_subscribers::{TelemetryConfig, TelemetryGuards};
use tracing::{Event, Metadata, Subscriber};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context as LayerContext, Layer};
use tracing_subscriber::registry::LookupSpan;

// Helper struct to visit and capture event fields as JSON
struct EventVisitor<'a> {
    fields: HashMap<&'a str, Value>,
    metadata: &'a Metadata<'a>,
}

impl<'a> EventVisitor<'a> {
    fn new(metadata: &'a Metadata<'a>) -> Self {
        EventVisitor {
            fields: HashMap::new(),
            metadata,
        }
    }
}

impl<'a> tracing::field::Visit for EventVisitor<'a> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name(), json!(format!("{:?}", value)));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.fields.insert(field.name(), json!(value));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields.insert(field.name(), json!(value));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields.insert(field.name(), json!(value));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields.insert(field.name(), json!(value));
    }
}

// Custom Layer to send logs to a log server
struct LogServerLayer {
    client: Client,
    log_server_url: String,
    level_filter: LevelFilter,
}

impl LogServerLayer {
    fn new(log_server_url: String, level_filter: LevelFilter) -> Self {
        LogServerLayer {
            client: Client::new(),
            log_server_url,
            level_filter,
        }
    }
}

impl<S> Layer<S> for LogServerLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, ctx: LayerContext<'_, S>) {
        // Check if the event's level is enabled by the filter
        if !self.level_filter.enabled(event.metadata(), ctx) {
            return; // Skip processing if level is not enabled
        }

        // Visit fields and collect them
        let mut visitor = EventVisitor::new(event.metadata());
        event.record(&mut visitor);

        // Construct the JSON payload
        let mut payload = json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "level": visitor.metadata.level().to_string(),
            "target": visitor.metadata.target(),
            "name": visitor.metadata.name(),
            "fields": visitor.fields,
        });

        // Extract "message" from fields for top-level convenience if it exists
        if let Some(fields_map) = payload["fields"].as_object_mut() {
            if let Some(message) = fields_map.remove("message") {
                if let Some(payload_map) = payload.as_object_mut() {
                    payload_map.insert("message".to_string(), message);
                }
            }
        }

        // Clone necessary data for the async task
        let client = self.client.clone();
        let url = self.log_server_url.clone();

        // Spawn a task to send the log asynchronously
        tokio::spawn(async move {
            let res = client.post(&url).json(&payload).send().await;
            if let Err(e) = res {
                // Use eprintln! for logging errors during logging setup/operation
                // to avoid infinitely looping if logging itself fails.
                eprintln!("Failed to send log to Log Server: {}", e);
                std::process::exit(1);
            }
        });
    }
}

impl LogServerLayer {
    async fn health_check(&self) -> Result<Response> {
        self.client
            .post(&self.log_server_url)
            .json(&json!({
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "level": "INFO",
                "target": "health_check",
                "name": "health_check",
            }))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send health check to Log Server: {}", e))
    }
}

pub async fn configure_logging(level: LevelFilter) -> Result<TelemetryGuards> {
    // Define the Log Server endpoint URL
    let log_server_endpoint =
        std::env::var("LOG_SERVER_ENDPOINT").unwrap_or("http://logserver:8080".to_string());

    // Create the custom layer
    let log_server_layer = LogServerLayer::new(log_server_endpoint, level);
    let telemetry_config = TelemetryConfig::new("nautilus-server").with_log_level("info");
    let guard = if log_server_layer
        .health_check()
        .await
        .context("Failed to send health check to Log Server")
        .is_ok()
    {
        let (guard, _) = telemetry_config.with_layer(log_server_layer).init();
        guard
    } else {
        // http log server is not available, so just log to stdout
        eprintln!("Failed to configure logging, continuing without http log publishing.");
        let (guard, _) = telemetry_config.init();
        guard
    };

    // Configure telemetry using the custom layer

    Ok(guard)
}
