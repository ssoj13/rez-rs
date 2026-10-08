// SPDX-License-Identifier: Apache-2.0
//! AMQP publisher for context tracking and release hooks.
//!
//! Publishes JSON messages to RabbitMQ per Python rez:
//! - Context tracking: action=created|sourced, payload from ResolvedContext
//! - Release hooks: REZ.PACKAGE.RELEASED (future)

use std::collections::HashMap;

use lapin::{options::*, BasicProperties, Connection, ConnectionProperties};
use serde_json::Value;

use crate::config::CONFIG;
use crate::errors::{Result, RezError};

/// Build AMQP URI from context_tracking_host and context_tracking_amqp.
/// Format: amqp://user:password@host:port/vhost
fn build_amqp_uri(host: &str, amqp: &HashMap<String, Value>) -> String {
    let user = amqp
        .get("userid")
        .and_then(|v| v.as_str())
        .unwrap_or("guest");
    let pass = amqp
        .get("password")
        .and_then(|v| v.as_str())
        .unwrap_or("guest");
    let vhost = amqp
        .get("virtual_host")
        .and_then(|v| v.as_str())
        .unwrap_or("/");

    let host_clean = host
        .trim_start_matches("amqp://")
        .trim_start_matches("amqps://");
    let (host_part, port) = if host_clean.contains(':') {
        let mut parts = host_clean.splitn(2, ':');
        let h = parts.next().unwrap_or("127.0.0.1");
        let p = parts
            .next()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(5672);
        (h, p)
    } else {
        (host_clean, 5672u16)
    };

    let vhost_encoded = if vhost == "/" {
        "%2f".to_string()
    } else {
        urlencoding::encode(vhost).to_string()
    };

    format!(
        "amqp://{}:{}@{}:{}/{}",
        urlencoding::encode(user),
        urlencoding::encode(pass),
        host_part,
        port,
        vhost_encoded
    )
}

/// Publish a JSON message to AMQP.
///
/// Uses config.context_tracking_amqp for exchange_name, exchange_routing_key,
/// message_delivery_mode. Blocking call.
pub fn publish_message(
    host: &str,
    amqp_settings: &HashMap<String, Value>,
    routing_key: &str,
    data: &Value,
) -> Result<bool> {
    if host == "stdout" {
        eprintln!("[AMQP] Published to {}: {}", routing_key, data);
        return Ok(true);
    }

    let uri = build_amqp_uri(host, amqp_settings);

    let exchange = amqp_settings
        .get("exchange_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let delivery_mode = amqp_settings
        .get("message_delivery_mode")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u8;

    let body = serde_json::to_vec(data)?;

    let props = BasicProperties::default()
        .with_content_type("application/json".into())
        .with_delivery_mode(delivery_mode);

    let result = async_global_executor::block_on(async {
        let conn = Connection::connect(&uri, ConnectionProperties::default()).await?;
        let channel = conn.create_channel().await?;
        channel
            .basic_publish(
                exchange.as_str().into(),
                routing_key.into(),
                BasicPublishOptions::default(),
                &body,
                props,
            )
            .await?;
        Ok::<(), lapin::Error>(())
    });

    match result {
        Ok(()) => Ok(true),
        Err(e) => {
            if CONFIG.debug_context_tracking {
                eprintln!("[rez-rs] AMQP publish failed: {}", e);
            }
            Err(RezError::System(format!("AMQP publish failed: {}", e)))
        }
    }
}

/// Publish context tracking payload (created/sourced).
/// Called from ResolvedContext::track_context.
pub fn publish_context_tracking(_action: &str, payload: &Value) -> Result<bool> {
    let host = CONFIG.context_tracking_host.trim();
    if host.is_empty() {
        return Ok(false);
    }

    let amqp = &CONFIG.context_tracking_amqp;
    let routing_key = amqp
        .get("exchange_routing_key")
        .and_then(|v| v.as_str())
        .unwrap_or("REZ.CONTEXT")
        .to_string();

    publish_message(host, amqp, &routing_key, payload)
}
