//! MQTT integration tests using a Mosquitto broker in Docker.
//!
//! These tests require a running Mosquitto broker started via:
//!   docker compose -f tests/docker-compose.mqtt.yml up -d
//!
//! Run with:
//!   cargo test --test mqtt_integration -- --ignored
//!
//! Or use the helper script:
//!   ./tests/run_mqtt_tests.sh

use chrono::Utc;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, TlsConfiguration, Transport};
use serde_json::json;
use std::time::Duration;
use tokio::time::timeout;

/// Broker ports (mapped from docker-compose)
const BROKER_TLS_PORT: u16 = 18883;
const BROKER_PLAIN_PORT: u16 = 11883;
const BROKER_HOST: &str = "localhost";

/// Test certificate paths (relative to CARGO_MANIFEST_DIR)
fn certs_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/certs")
}

fn load_test_tls_config() -> TlsConfiguration {
    let dir = certs_dir();
    let ca = std::fs::read(dir.join("ca.pem")).expect("ca.pem");
    let cert = std::fs::read(dir.join("client.pem")).expect("client.pem");
    let key = std::fs::read(dir.join("client.key")).expect("client.key");
    TlsConfiguration::Simple {
        ca,
        alpn: None,
        client_auth: Some((cert, key)),
    }
}

fn make_tls_client(client_id: &str) -> (AsyncClient, rumqttc::EventLoop) {
    let mut opts = MqttOptions::new(client_id, BROKER_HOST, BROKER_TLS_PORT);
    opts.set_keep_alive(Duration::from_secs(10));
    opts.set_transport(Transport::tls_with_config(load_test_tls_config()));
    opts.set_clean_session(true);
    AsyncClient::new(opts, 10)
}

fn make_plain_client(client_id: &str) -> (AsyncClient, rumqttc::EventLoop) {
    let mut opts = MqttOptions::new(client_id, BROKER_HOST, BROKER_PLAIN_PORT);
    opts.set_keep_alive(Duration::from_secs(10));
    opts.set_clean_session(true);
    AsyncClient::new(opts, 10)
}

/// Wait for ConnAck on the event loop, returning the event loop for further use.
async fn wait_for_connect(eventloop: &mut rumqttc::EventLoop, label: &str) {
    let deadline = Duration::from_secs(10);
    let start = tokio::time::Instant::now();
    loop {
        let remaining = deadline.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            panic!("{label}: timed out waiting for ConnAck");
        }
        match timeout(remaining, eventloop.poll()).await {
            Ok(Ok(Event::Incoming(Packet::ConnAck(ack)))) => {
                assert_eq!(
                    ack.code,
                    rumqttc::ConnectReturnCode::Success,
                    "{label}: unexpected ConnAck code: {:?}",
                    ack.code
                );
                return;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => panic!("{label}: connection error: {e}"),
            Err(_) => panic!("{label}: timed out waiting for ConnAck"),
        }
    }
}

/// Poll event loop until we get a Publish on the expected topic, with a timeout.
async fn wait_for_publish(
    eventloop: &mut rumqttc::EventLoop,
    expected_topic: &str,
    timeout_dur: Duration,
) -> Vec<u8> {
    let start = tokio::time::Instant::now();
    loop {
        let remaining = timeout_dur.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            panic!("timed out waiting for publish on {expected_topic}");
        }
        match timeout(remaining, eventloop.poll()).await {
            Ok(Ok(Event::Incoming(Packet::Publish(p)))) if p.topic == expected_topic => {
                return p.payload.to_vec();
            }
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => panic!("event loop error while waiting for {expected_topic}: {e}"),
            Err(_) => panic!("timed out waiting for publish on {expected_topic}"),
        }
    }
}

/// Collect all publishes on any topic matching prefix within a timeout window.
async fn collect_publishes(
    eventloop: &mut rumqttc::EventLoop,
    topic_prefix: &str,
    collect_duration: Duration,
) -> Vec<(String, Vec<u8>)> {
    let mut messages = Vec::new();
    let start = tokio::time::Instant::now();
    loop {
        let remaining = collect_duration.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, eventloop.poll()).await {
            Ok(Ok(Event::Incoming(Packet::Publish(p)))) if p.topic.starts_with(topic_prefix) => {
                messages.push((p.topic.clone(), p.payload.to_vec()));
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    messages
}

// ───────────────────── Test: TLS Connection ─────────────────────

#[tokio::test]
#[ignore = "requires docker mosquitto broker"]
async fn test_tls_connection_established() {
    let (_client, mut eventloop) = make_tls_client("integ-tls-connect");
    wait_for_connect(&mut eventloop, "tls-connect").await;
}

// ───────────────────── Test: Telemetry Pub/Sub ─────────────────────

#[tokio::test]
#[ignore = "requires docker mosquitto broker"]
async fn test_telemetry_publish_subscribe() {
    let env_prefix = "test";
    let node_id = "test-drone-01";
    let telemetry_topic = format!("{env_prefix}/nodes/{node_id}/telemetry/lte");

    // Subscriber client
    let (sub_client, mut sub_eventloop) = make_tls_client("integ-telem-sub");
    wait_for_connect(&mut sub_eventloop, "sub").await;
    sub_client
        .subscribe(&telemetry_topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    // Drain SubAck
    let _ = timeout(Duration::from_secs(2), sub_eventloop.poll()).await;

    // Publisher client
    let (pub_client, mut pub_eventloop) = make_tls_client("integ-telem-pub");
    wait_for_connect(&mut pub_eventloop, "pub").await;

    let payload = json!({
        "ts": "2026-03-23T10:04:00Z",
        "rsrq": -10,
        "rsrp": -85,
        "rssi": -60,
        "rssnr": 15,
        "earfcn": 1300,
        "tx_power": 23,
        "pcid": 42
    });
    let payload_bytes = serde_json::to_vec(&payload).unwrap();

    pub_client
        .publish(
            &telemetry_topic,
            QoS::AtLeastOnce,
            false,
            payload_bytes.clone(),
        )
        .await
        .unwrap();
    // Drive publisher event loop to send the message
    let _ = timeout(Duration::from_secs(2), pub_eventloop.poll()).await;

    // Receive on subscriber
    let received =
        wait_for_publish(&mut sub_eventloop, &telemetry_topic, Duration::from_secs(5)).await;
    let received_json: serde_json::Value = serde_json::from_slice(&received).unwrap();
    assert_eq!(received_json["rsrp"], -85);
    assert_eq!(received_json["rssi"], -60);
    assert!(received_json["ts"].is_string());
}

// ───────────────────── Test: Command Lifecycle ─────────────────────

#[tokio::test]
#[ignore = "requires docker mosquitto broker"]
async fn test_command_lifecycle_status_transitions() {
    // Simulates the full command lifecycle:
    // Server sends command on .../in -> drone publishes status (accepted, in_progress, completed) and result

    let env_prefix = "test";
    let node_id = "test-drone-01";
    let cmd_name = "get_config";
    let in_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}/in");
    let status_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}/status");
    let result_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}/result");

    // "Server" client: subscribes to status + result, publishes command on /in
    let (server_client, mut server_eventloop) = make_tls_client("integ-cmd-server");
    wait_for_connect(&mut server_eventloop, "server").await;
    server_client
        .subscribe(&status_topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    server_client
        .subscribe(&result_topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    // Drain SubAck events
    let _ = timeout(Duration::from_secs(2), server_eventloop.poll()).await;
    let _ = timeout(Duration::from_secs(1), server_eventloop.poll()).await;

    // "Drone" client: subscribes to command /in topic, simulates processing
    let (drone_client, mut drone_eventloop) = make_tls_client("integ-cmd-drone");
    wait_for_connect(&mut drone_eventloop, "drone").await;
    drone_client
        .subscribe(&in_topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(2), drone_eventloop.poll()).await;

    // Server publishes command
    let command = json!({
        "uuid": "cmd-1234",
        "issued_at": Utc::now().to_rfc3339(),
        "ttl_sec": 300,
        "payload": {}
    });
    server_client
        .publish(
            &in_topic,
            QoS::AtLeastOnce,
            false,
            serde_json::to_vec(&command).unwrap(),
        )
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(2), server_eventloop.poll()).await;

    // Drone receives command
    let cmd_payload =
        wait_for_publish(&mut drone_eventloop, &in_topic, Duration::from_secs(5)).await;
    let cmd_json: serde_json::Value = serde_json::from_slice(&cmd_payload).unwrap();
    assert_eq!(cmd_json["uuid"], "cmd-1234");

    // Drone publishes status transitions
    let accepted = json!({"state": "accepted", "uuid": "cmd-1234"});
    drone_client
        .publish(
            &status_topic,
            QoS::AtLeastOnce,
            false,
            serde_json::to_vec(&accepted).unwrap(),
        )
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(1), drone_eventloop.poll()).await;

    let in_progress = json!({"state": "in_progress", "uuid": "cmd-1234"});
    drone_client
        .publish(
            &status_topic,
            QoS::AtLeastOnce,
            false,
            serde_json::to_vec(&in_progress).unwrap(),
        )
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(1), drone_eventloop.poll()).await;

    let completed = json!({"state": "completed", "uuid": "cmd-1234"});
    drone_client
        .publish(
            &status_topic,
            QoS::AtLeastOnce,
            false,
            serde_json::to_vec(&completed).unwrap(),
        )
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(1), drone_eventloop.poll()).await;

    // Drone publishes result
    let result = json!({"ok": true, "ts": Utc::now().to_rfc3339(), "config": {"enabled": true}});
    drone_client
        .publish(
            &result_topic,
            QoS::AtLeastOnce,
            false,
            serde_json::to_vec(&result).unwrap(),
        )
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(1), drone_eventloop.poll()).await;

    // Server verifies it receives the status and result messages
    let messages = collect_publishes(
        &mut server_eventloop,
        &format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}"),
        Duration::from_secs(5),
    )
    .await;

    let status_messages: Vec<_> = messages
        .iter()
        .filter(|(t, _)| t == &status_topic)
        .map(|(_, p)| serde_json::from_slice::<serde_json::Value>(p).unwrap())
        .collect();

    let result_messages: Vec<_> = messages
        .iter()
        .filter(|(t, _)| t == &result_topic)
        .map(|(_, p)| serde_json::from_slice::<serde_json::Value>(p).unwrap())
        .collect();

    // Verify we got the status transitions
    assert!(
        status_messages.iter().any(|m| m["state"] == "accepted"),
        "missing accepted status, got: {status_messages:?}"
    );
    assert!(
        status_messages.iter().any(|m| m["state"] == "in_progress"),
        "missing in_progress status, got: {status_messages:?}"
    );
    assert!(
        status_messages.iter().any(|m| m["state"] == "completed"),
        "missing completed status, got: {status_messages:?}"
    );

    // Verify result
    assert!(!result_messages.is_empty(), "no result messages received");
    assert_eq!(result_messages[0]["ok"], true);
}

// ───────────────────── Test: Expired Command Topic Routing ─────────────────────

#[tokio::test]
#[ignore = "requires docker mosquitto broker"]
async fn test_expired_command_topic_routing() {
    // Verifies that command and status messages round-trip through the broker
    // on the expected expired-command topic structure. Note: this test manually
    // publishes the "expired" status — it does not test CommandProcessor logic.

    let env_prefix = "test";
    let node_id = "test-drone-01";
    let cmd_name = "config_update";
    let in_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}/in");
    let status_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}/status");

    // Subscriber watches status topic
    let (sub_client, mut sub_eventloop) = make_tls_client("integ-expired-sub");
    wait_for_connect(&mut sub_eventloop, "sub").await;
    sub_client
        .subscribe(&status_topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(2), sub_eventloop.poll()).await;

    // Publisher sends expired command on /in and also the expected expired status
    let (pub_client, mut pub_eventloop) = make_tls_client("integ-expired-pub");
    wait_for_connect(&mut pub_eventloop, "pub").await;

    // Publish the expired command
    let expired_cmd = json!({
        "uuid": "expired-cmd-001",
        "issued_at": "2020-01-01T00:00:00Z",
        "ttl_sec": 1,
        "payload": {"key": "value"}
    });
    pub_client
        .publish(
            &in_topic,
            QoS::AtLeastOnce,
            false,
            serde_json::to_vec(&expired_cmd).unwrap(),
        )
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(2), pub_eventloop.poll()).await;

    // Simulate drone detecting expiry and publishing expired status
    let expired_status = json!({"state": "expired", "uuid": "expired-cmd-001"});
    pub_client
        .publish(
            &status_topic,
            QoS::AtLeastOnce,
            false,
            serde_json::to_vec(&expired_status).unwrap(),
        )
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(2), pub_eventloop.poll()).await;

    // Subscriber receives the expired status
    let payload = wait_for_publish(&mut sub_eventloop, &status_topic, Duration::from_secs(5)).await;
    let status: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(status["state"], "expired");
    assert_eq!(status["uuid"], "expired-cmd-001");
}

// ───────────────────── Test: Rejected Command Topic Routing ─────────────────────

#[tokio::test]
#[ignore = "requires docker mosquitto broker"]
async fn test_rejected_command_topic_routing() {
    // Verifies that command and status messages round-trip through the broker
    // on the expected rejected-command topic structure. Note: this test manually
    // publishes the "rejected" status — it does not test CommandProcessor logic.

    let env_prefix = "test";
    let node_id = "test-drone-01";
    let cmd_name = "nonexistent_command";
    let in_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}/in");
    let status_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}/status");

    let (sub_client, mut sub_eventloop) = make_tls_client("integ-rejected-sub");
    wait_for_connect(&mut sub_eventloop, "sub").await;
    sub_client
        .subscribe(&status_topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(2), sub_eventloop.poll()).await;

    let (pub_client, mut pub_eventloop) = make_tls_client("integ-rejected-pub");
    wait_for_connect(&mut pub_eventloop, "pub").await;

    // Publish command for unknown handler
    let cmd = json!({
        "uuid": "rejected-cmd-001",
        "issued_at": Utc::now().to_rfc3339(),
        "ttl_sec": 300,
        "payload": {}
    });
    pub_client
        .publish(
            &in_topic,
            QoS::AtLeastOnce,
            false,
            serde_json::to_vec(&cmd).unwrap(),
        )
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(2), pub_eventloop.poll()).await;

    // Simulate drone publishing rejected status
    let rejected_status = json!({"state": "rejected", "uuid": "rejected-cmd-001"});
    pub_client
        .publish(
            &status_topic,
            QoS::AtLeastOnce,
            false,
            serde_json::to_vec(&rejected_status).unwrap(),
        )
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(2), pub_eventloop.poll()).await;

    let payload = wait_for_publish(&mut sub_eventloop, &status_topic, Duration::from_secs(5)).await;
    let status: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(status["state"], "rejected");
    assert_eq!(status["uuid"], "rejected-cmd-001");
}

// ───────────────────── Test: Sequential Publish ─────────────────────

#[tokio::test]
#[ignore = "requires docker mosquitto broker"]
async fn test_sequential_publish_delivery() {
    // Verifies that multiple messages published sequentially on the same connection
    // are all delivered to the subscriber. Note: this does not test actual broker
    // disconnect/reconnect — it tests sequential message delivery.

    let topic = "test/nodes/test-drone-01/telemetry/reconnect_test";

    // Use plain TCP for simpler reconnect testing
    let (sub_client, mut sub_eventloop) = make_plain_client("integ-reconn-sub");
    wait_for_connect(&mut sub_eventloop, "sub").await;
    sub_client.subscribe(topic, QoS::AtLeastOnce).await.unwrap();
    let _ = timeout(Duration::from_secs(2), sub_eventloop.poll()).await;

    let (pub_client, mut pub_eventloop) = make_plain_client("integ-reconn-pub");
    wait_for_connect(&mut pub_eventloop, "pub").await;

    // Publish first message
    pub_client
        .publish(topic, QoS::AtLeastOnce, false, b"msg-before".to_vec())
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(2), pub_eventloop.poll()).await;

    let payload1 = wait_for_publish(&mut sub_eventloop, topic, Duration::from_secs(5)).await;
    assert_eq!(payload1, b"msg-before");

    // Publish second message (simulates resume after reconnect)
    pub_client
        .publish(topic, QoS::AtLeastOnce, false, b"msg-after".to_vec())
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(2), pub_eventloop.poll()).await;

    let payload2 = wait_for_publish(&mut sub_eventloop, topic, Duration::from_secs(5)).await;
    assert_eq!(payload2, b"msg-after");
}

// ───────────────────── Test: Wildcard Subscription ─────────────────────

#[tokio::test]
#[ignore = "requires docker mosquitto broker"]
async fn test_wildcard_command_subscription() {
    // Test that subscribing with '+' wildcard receives messages on different command topics,
    // which is how CommandProcessor subscribes to all commands.

    let env_prefix = "test";
    let node_id = "test-drone-01";
    let wildcard_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/+/in");

    let (sub_client, mut sub_eventloop) = make_tls_client("integ-wildcard-sub");
    wait_for_connect(&mut sub_eventloop, "sub").await;
    sub_client
        .subscribe(&wildcard_topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(2), sub_eventloop.poll()).await;

    let (pub_client, mut pub_eventloop) = make_tls_client("integ-wildcard-pub");
    wait_for_connect(&mut pub_eventloop, "pub").await;

    // Publish to two different command topics
    let cmd1_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/get_config/in");
    let cmd2_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/update_request/in");

    let cmd = json!({"uuid": "wc-1", "issued_at": Utc::now().to_rfc3339(), "ttl_sec": 300, "payload": {}});
    pub_client
        .publish(
            &cmd1_topic,
            QoS::AtLeastOnce,
            false,
            serde_json::to_vec(&cmd).unwrap(),
        )
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(1), pub_eventloop.poll()).await;

    let cmd = json!({"uuid": "wc-2", "issued_at": Utc::now().to_rfc3339(), "ttl_sec": 300, "payload": {}});
    pub_client
        .publish(
            &cmd2_topic,
            QoS::AtLeastOnce,
            false,
            serde_json::to_vec(&cmd).unwrap(),
        )
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(1), pub_eventloop.poll()).await;

    // Subscriber should receive both via wildcard
    let messages = collect_publishes(
        &mut sub_eventloop,
        &format!("{env_prefix}/nodes/{node_id}/cmnd/"),
        Duration::from_secs(5),
    )
    .await;

    let topics: Vec<_> = messages.iter().map(|(t, _)| t.as_str()).collect();
    assert!(
        topics.contains(&cmd1_topic.as_str()),
        "missing get_config command, got topics: {topics:?}"
    );
    assert!(
        topics.contains(&cmd2_topic.as_str()),
        "missing update_request command, got topics: {topics:?}"
    );
}
