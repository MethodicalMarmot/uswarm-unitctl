//! MQTT integration tests using a Mosquitto broker via testcontainers.
//!
//! No external scripts or manual docker-compose needed. Run with:
//!   cargo test --test mqtt_integration

use chrono::Utc;
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose, SanType};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, TlsConfiguration, Transport};
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;
use testcontainers::core::{IntoContainerPort, Mount, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::time::timeout;

/// Ports inside the container (must match mosquitto.conf).
const CONTAINER_TLS_PORT: u16 = 8883;
const CONTAINER_PLAIN_PORT: u16 = 1883;
const BROKER_HOST: &str = "localhost";

// ───────────────────── Certificate generation ─────────────────────

struct TestCerts {
    _dir: TempDir,
    ca_pem: Vec<u8>,
    client_pem: Vec<u8>,
    client_key: Vec<u8>,
    certs_path: PathBuf,
}

fn generate_test_certs() -> TestCerts {
    let dir = TempDir::new().expect("failed to create temp dir");
    let certs_path = dir.path().to_path_buf();

    // CA
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::new()).unwrap();
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "Test MQTT CA");
    ca_params
        .distinguished_name
        .push(DnType::OrganizationName, "UnitctlTest");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    let ca_pem = ca_cert.pem().into_bytes();
    let ca_key_pem = ca_key.serialize_pem();

    std::fs::write(certs_path.join("ca.pem"), &ca_pem).unwrap();
    std::fs::write(certs_path.join("ca.key"), &ca_key_pem).unwrap();

    // Server cert
    let server_key = KeyPair::generate().unwrap();
    let mut server_params = CertificateParams::new(Vec::new()).unwrap();
    server_params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    server_params
        .distinguished_name
        .push(DnType::OrganizationName, "UnitctlTest");
    server_params.subject_alt_names = vec![
        SanType::DnsName("localhost".try_into().unwrap()),
        SanType::IpAddress(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))),
    ];
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .unwrap();

    std::fs::write(certs_path.join("server.pem"), server_cert.pem()).unwrap();
    std::fs::write(certs_path.join("server.key"), server_key.serialize_pem()).unwrap();

    // Client cert (CN = node ID)
    let client_key = KeyPair::generate().unwrap();
    let mut client_params = CertificateParams::new(Vec::new()).unwrap();
    client_params
        .distinguished_name
        .push(DnType::CommonName, "test-drone-01");
    client_params
        .distinguished_name
        .push(DnType::OrganizationName, "UnitctlTest");
    let client_cert = client_params
        .signed_by(&client_key, &ca_cert, &ca_key)
        .unwrap();

    let client_pem = client_cert.pem().into_bytes();
    let client_key_pem = client_key.serialize_pem().into_bytes();

    std::fs::write(certs_path.join("client.pem"), &client_pem).unwrap();
    std::fs::write(certs_path.join("client.key"), &client_key_pem).unwrap();

    // Mosquitto config
    let mosquitto_conf = "\
allow_anonymous true

listener 8883
cafile /mosquitto/certs/ca.pem
certfile /mosquitto/certs/server.pem
keyfile /mosquitto/certs/server.key
require_certificate true
use_identity_as_username true
tls_version tlsv1.2

listener 1883
";
    std::fs::write(certs_path.join("mosquitto.conf"), mosquitto_conf).unwrap();

    TestCerts {
        _dir: dir,
        ca_pem,
        client_pem,
        client_key: client_key_pem,
        certs_path,
    }
}

// ───────────────────── Container setup ─────────────────────

struct MosquittoContainer {
    _certs: TestCerts,
    _container: ContainerAsync<GenericImage>,
    tls_port: u16,
    plain_port: u16,
}

impl MosquittoContainer {
    fn tls_config(&self) -> TlsConfiguration {
        TlsConfiguration::Simple {
            ca: self._certs.ca_pem.clone(),
            alpn: None,
            client_auth: Some((
                self._certs.client_pem.clone(),
                self._certs.client_key.clone(),
            )),
        }
    }

    fn make_tls_client(&self, client_id: &str) -> (AsyncClient, rumqttc::EventLoop) {
        let mut opts = MqttOptions::new(client_id, BROKER_HOST, self.tls_port);
        opts.set_keep_alive(Duration::from_secs(10));
        opts.set_transport(Transport::tls_with_config(self.tls_config()));
        opts.set_clean_session(true);
        AsyncClient::new(opts, 10)
    }

    fn make_plain_client(&self, client_id: &str) -> (AsyncClient, rumqttc::EventLoop) {
        let mut opts = MqttOptions::new(client_id, BROKER_HOST, self.plain_port);
        opts.set_keep_alive(Duration::from_secs(10));
        opts.set_clean_session(true);
        AsyncClient::new(opts, 10)
    }
}

async fn start_mosquitto() -> MosquittoContainer {
    let certs = generate_test_certs();
    let certs_path = certs.certs_path.clone();

    let image = GenericImage::new("eclipse-mosquitto", "2")
        .with_exposed_port(CONTAINER_TLS_PORT.tcp())
        .with_exposed_port(CONTAINER_PLAIN_PORT.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Opening ipv4 listen socket on port 1883"));

    let container = image
        .with_mount(Mount::bind_mount(
            certs_path.join("mosquitto.conf").to_str().unwrap(),
            "/mosquitto/config/mosquitto.conf",
        ))
        .with_mount(Mount::bind_mount(
            certs_path.join("ca.pem").to_str().unwrap(),
            "/mosquitto/certs/ca.pem",
        ))
        .with_mount(Mount::bind_mount(
            certs_path.join("server.pem").to_str().unwrap(),
            "/mosquitto/certs/server.pem",
        ))
        .with_mount(Mount::bind_mount(
            certs_path.join("server.key").to_str().unwrap(),
            "/mosquitto/certs/server.key",
        ))
        .with_startup_timeout(Duration::from_secs(30))
        .start()
        .await
        .expect("failed to start mosquitto container");

    let tls_port = container
        .get_host_port_ipv4(CONTAINER_TLS_PORT)
        .await
        .unwrap();
    let plain_port = container
        .get_host_port_ipv4(CONTAINER_PLAIN_PORT)
        .await
        .unwrap();

    MosquittoContainer {
        _certs: certs,
        _container: container,
        tls_port,
        plain_port,
    }
}

// ───────────────────── Helpers ─────────────────────

/// Drain the event loop until we see a SubAck, ensuring the broker has processed
/// the subscription before we publish.
async fn wait_for_suback(eventloop: &mut rumqttc::EventLoop, label: &str) {
    let deadline = Duration::from_secs(5);
    let start = tokio::time::Instant::now();
    loop {
        let remaining = deadline.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            panic!("{label}: timed out waiting for SubAck");
        }
        match timeout(remaining, eventloop.poll()).await {
            Ok(Ok(Event::Incoming(Packet::SubAck(_)))) => return,
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => panic!("{label}: error waiting for SubAck: {e}"),
            Err(_) => panic!("{label}: timed out waiting for SubAck"),
        }
    }
}

/// Drive the event loop until a PubAck is received, ensuring the message was
/// actually sent to the broker before we try to receive it on the subscriber.
async fn wait_for_puback(eventloop: &mut rumqttc::EventLoop, label: &str) {
    let deadline = Duration::from_secs(5);
    let start = tokio::time::Instant::now();
    loop {
        let remaining = deadline.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            panic!("{label}: timed out waiting for PubAck");
        }
        match timeout(remaining, eventloop.poll()).await {
            Ok(Ok(Event::Incoming(Packet::PubAck(_)))) => return,
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => panic!("{label}: error waiting for PubAck: {e}"),
            Err(_) => panic!("{label}: timed out waiting for PubAck"),
        }
    }
}

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
async fn test_tls_connection_established() {
    let broker = start_mosquitto().await;
    let (_client, mut eventloop) = broker.make_tls_client("integ-tls-connect");
    wait_for_connect(&mut eventloop, "tls-connect").await;
}

// ───────────────────── Test: Telemetry Pub/Sub ─────────────────────

#[tokio::test]
async fn test_telemetry_publish_subscribe() {
    let broker = start_mosquitto().await;

    let env_prefix = "test";
    let node_id = "test-drone-01";
    let telemetry_topic = format!("{env_prefix}/nodes/{node_id}/telemetry/lte");

    // Subscriber client
    let (sub_client, mut sub_eventloop) = broker.make_tls_client("integ-telem-sub");
    wait_for_connect(&mut sub_eventloop, "sub").await;
    sub_client
        .subscribe(&telemetry_topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    wait_for_suback(&mut sub_eventloop, "sub").await;

    // Publisher client
    let (pub_client, mut pub_eventloop) = broker.make_tls_client("integ-telem-pub");
    wait_for_connect(&mut pub_eventloop, "pub").await;

    let payload = json!({
        "ts": "2026-03-23T10:04:00Z",
        "data": {
            "type": "Lte",
            "signal": {
                "rsrq": -10,
                "rsrp": -85,
                "rssi": -60,
                "rssnr": 15,
                "earfcn": 1300,
                "tx_power": 23,
                "pcid": 42
            },
            "neighbors": []
        }
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
    assert_eq!(received_json["data"]["signal"]["rsrp"], -85);
    assert_eq!(received_json["data"]["signal"]["rssi"], -60);
    assert!(received_json["ts"].is_string());
}

// ───────────────────── Test: Command Lifecycle ─────────────────────

#[tokio::test]
async fn test_command_lifecycle_status_transitions() {
    let broker = start_mosquitto().await;

    let env_prefix = "test";
    let node_id = "test-drone-01";
    let cmd_name = "get_config";
    let in_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}/in");
    let status_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}/status");
    let result_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}/result");

    // "Server" client: subscribes to status + result, publishes command on /in
    let (server_client, mut server_eventloop) = broker.make_tls_client("integ-cmd-server");
    wait_for_connect(&mut server_eventloop, "server").await;
    server_client
        .subscribe(&status_topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    server_client
        .subscribe(&result_topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    wait_for_suback(&mut server_eventloop, "server-status").await;
    wait_for_suback(&mut server_eventloop, "server-result").await;

    // "Drone" client: subscribes to command /in topic, simulates processing
    let (drone_client, mut drone_eventloop) = broker.make_tls_client("integ-cmd-drone");
    wait_for_connect(&mut drone_eventloop, "drone").await;
    drone_client
        .subscribe(&in_topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    wait_for_suback(&mut drone_eventloop, "drone-in").await;

    // Server publishes command
    let command = json!({
        "uuid": "cmd-1234",
        "issued_at": Utc::now().to_rfc3339(),
        "ttl_sec": 300,
        "payload": {"type": "GetConfig"}
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

    // Drone publishes status transitions (matching CommandStatus schema: uuid, state, ts)
    let accepted = json!({"state": "accepted", "uuid": "cmd-1234", "ts": Utc::now().to_rfc3339()});
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

    let in_progress = json!({"state": "in_progress", "uuid": "cmd-1234", "ts": Utc::now().to_rfc3339()});
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

    let completed = json!({"state": "completed", "uuid": "cmd-1234", "ts": Utc::now().to_rfc3339()});
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

    // Drone publishes result (matching CommandResultMsg schema with full SafeConfig)
    let result = json!({
        "uuid": "cmd-1234",
        "ok": true,
        "ts": Utc::now().to_rfc3339(),
        "data": {
            "type": "GetConfig",
            "config": {
                "general": {"debug": false},
                "mavlink": {
                    "protocol": "tcpout", "host": "127.0.0.1",
                    "local_mavlink_port": 5760, "remote_mavlink_port": 5760,
                    "self_sysid": 1, "self_compid": 10,
                    "gcs_sysid": 255, "gcs_compid": 190,
                    "sniffer_sysid": 199, "bs_sysid": 200,
                    "iteration_period_ms": 10, "gcs_ip": "10.101.0.1",
                    "env_path": "/etc/mavlink.env",
                    "fc": {"tty": "/dev/ttyFC", "baudrate": 57600}
                },
                "sensors": {
                    "default_interval_s": 1.0,
                    "ping": {"enabled": true, "host": "10.45.0.2", "interface": ""},
                    "lte": {"enabled": true, "neighbor_expiry_s": 30.0},
                    "cpu_temp": {"enabled": true}
                },
                "camera": {
                    "gcs_ip": "10.101.0.1", "env_path": "/etc/camera.env",
                    "remote_video_port": 5600, "width": 640, "height": 360,
                    "framerate": 60, "bitrate": 1664000, "flip": 0,
                    "camera_type": "rpi", "device": "/dev/video1"
                },
                "mqtt": {
                    "enabled": false, "host": "mqtt.example.com", "port": 8883,
                    "ca_cert_path": "***", "client_cert_path": "***",
                    "client_key_path": "***", "env_prefix": "test",
                    "telemetry_interval_s": 1.0
                }
            }
        }
    });
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
async fn test_expired_command_topic_routing() {
    let broker = start_mosquitto().await;

    let env_prefix = "test";
    let node_id = "test-drone-01";
    let cmd_name = "config_update";
    let in_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}/in");
    let status_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}/status");

    // Subscriber watches status topic
    let (sub_client, mut sub_eventloop) = broker.make_tls_client("integ-expired-sub");
    wait_for_connect(&mut sub_eventloop, "sub").await;
    sub_client
        .subscribe(&status_topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    wait_for_suback(&mut sub_eventloop, "expired-sub").await;

    // Publisher sends expired command on /in and also the expected expired status
    let (pub_client, mut pub_eventloop) = broker.make_tls_client("integ-expired-pub");
    wait_for_connect(&mut pub_eventloop, "pub").await;

    // Publish the expired command (ConfigUpdate payload matches config_update topic)
    let expired_cmd = json!({
        "uuid": "expired-cmd-001",
        "issued_at": "2020-01-01T00:00:00Z",
        "ttl_sec": 1,
        "payload": {"type": "ConfigUpdate", "payload": {"sensors": {"ping": {"enabled": false}}}}
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

    // Simulate drone detecting expiry and publishing expired status (matching CommandStatus schema)
    let expired_status = json!({"state": "expired", "uuid": "expired-cmd-001", "ts": Utc::now().to_rfc3339()});
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
    let payload =
        wait_for_publish(&mut sub_eventloop, &status_topic, Duration::from_secs(5)).await;
    let status: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(status["state"], "expired");
    assert_eq!(status["uuid"], "expired-cmd-001");
}

// ───────────────────── Test: Rejected Command Topic Routing ─────────────────────

#[tokio::test]
async fn test_rejected_command_topic_routing() {
    let broker = start_mosquitto().await;

    let env_prefix = "test";
    let node_id = "test-drone-01";
    let cmd_name = "nonexistent_command";
    let in_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}/in");
    let status_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/{cmd_name}/status");

    let (sub_client, mut sub_eventloop) = broker.make_tls_client("integ-rejected-sub");
    wait_for_connect(&mut sub_eventloop, "sub").await;
    sub_client
        .subscribe(&status_topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    wait_for_suback(&mut sub_eventloop, "rejected-sub").await;

    let (pub_client, mut pub_eventloop) = broker.make_tls_client("integ-rejected-pub");
    wait_for_connect(&mut pub_eventloop, "pub").await;

    // Publish command for unknown handler
    let cmd = json!({
        "uuid": "rejected-cmd-001",
        "issued_at": Utc::now().to_rfc3339(),
        "ttl_sec": 300,
        "payload": {"type": "GetConfig"}
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

    // Simulate drone publishing rejected status (matching CommandStatus schema)
    let rejected_status = json!({"state": "rejected", "uuid": "rejected-cmd-001", "ts": Utc::now().to_rfc3339()});
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

    let payload =
        wait_for_publish(&mut sub_eventloop, &status_topic, Duration::from_secs(5)).await;
    let status: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(status["state"], "rejected");
    assert_eq!(status["uuid"], "rejected-cmd-001");
}

// ───────────────────── Test: Sequential Publish ─────────────────────

#[tokio::test]
async fn test_sequential_publish_delivery() {
    let broker = start_mosquitto().await;

    let topic = "test/nodes/test-drone-01/telemetry/reconnect_test";

    // Use plain TCP for simpler reconnect testing
    let (sub_client, mut sub_eventloop) = broker.make_plain_client("integ-reconn-sub");
    wait_for_connect(&mut sub_eventloop, "sub").await;
    sub_client.subscribe(topic, QoS::AtLeastOnce).await.unwrap();
    wait_for_suback(&mut sub_eventloop, "reconn-sub").await;

    let (pub_client, mut pub_eventloop) = broker.make_plain_client("integ-reconn-pub");
    wait_for_connect(&mut pub_eventloop, "pub").await;

    // Publish first message
    pub_client
        .publish(topic, QoS::AtLeastOnce, false, b"msg-before".to_vec())
        .await
        .unwrap();
    wait_for_puback(&mut pub_eventloop, "pub-before").await;

    let payload1 = wait_for_publish(&mut sub_eventloop, topic, Duration::from_secs(5)).await;
    assert_eq!(payload1, b"msg-before");

    // Publish second message (simulates resume after reconnect)
    pub_client
        .publish(topic, QoS::AtLeastOnce, false, b"msg-after".to_vec())
        .await
        .unwrap();
    wait_for_puback(&mut pub_eventloop, "pub-after").await;

    let payload2 = wait_for_publish(&mut sub_eventloop, topic, Duration::from_secs(5)).await;
    assert_eq!(payload2, b"msg-after");
}

// ───────────────────── Test: Wildcard Subscription ─────────────────────

#[tokio::test]
async fn test_wildcard_command_subscription() {
    let broker = start_mosquitto().await;

    let env_prefix = "test";
    let node_id = "test-drone-01";
    let wildcard_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/+/in");

    let (sub_client, mut sub_eventloop) = broker.make_tls_client("integ-wildcard-sub");
    wait_for_connect(&mut sub_eventloop, "sub").await;
    sub_client
        .subscribe(&wildcard_topic, QoS::AtLeastOnce)
        .await
        .unwrap();
    wait_for_suback(&mut sub_eventloop, "wildcard-sub").await;

    let (pub_client, mut pub_eventloop) = broker.make_tls_client("integ-wildcard-pub");
    wait_for_connect(&mut pub_eventloop, "pub").await;

    // Publish to two different command topics
    let cmd1_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/get_config/in");
    let cmd2_topic = format!("{env_prefix}/nodes/{node_id}/cmnd/update_request/in");

    let cmd = json!({"uuid": "wc-1", "issued_at": Utc::now().to_rfc3339(), "ttl_sec": 300, "payload": {"type": "GetConfig"}});
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

    let cmd = json!({"uuid": "wc-2", "issued_at": Utc::now().to_rfc3339(), "ttl_sec": 300, "payload": {"type": "UpdateRequest", "version": "1.0.0", "url": "https://example.com/update"}});
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
