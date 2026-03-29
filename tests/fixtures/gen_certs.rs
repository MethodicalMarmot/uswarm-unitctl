// Standalone certificate generator for MQTT integration test fixtures.
// Run with: cargo run --example gen_certs (or via the generate_certs.sh wrapper)
// Generates CA, server, client, and admin certificates for mTLS testing.

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose, SanType};
use std::fs;
use std::path::Path;

fn main() {
    let certs_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/certs");
    fs::create_dir_all(&certs_dir).unwrap();

    // Generate CA
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

    fs::write(certs_dir.join("ca.pem"), ca_cert.pem()).unwrap();
    fs::write(certs_dir.join("ca.key"), ca_key.serialize_pem()).unwrap();

    // Generate server cert signed by CA
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
        SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))),
    ];
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .unwrap();

    fs::write(certs_dir.join("server.pem"), server_cert.pem()).unwrap();
    fs::write(certs_dir.join("server.key"), server_key.serialize_pem()).unwrap();

    // Generate client cert signed by CA (CN = node ID)
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

    fs::write(certs_dir.join("client.pem"), client_cert.pem()).unwrap();
    fs::write(certs_dir.join("client.key"), client_key.serialize_pem()).unwrap();

    // Generate admin cert signed by CA (CN = admin, full topic access)
    let admin_key = KeyPair::generate().unwrap();
    let mut admin_params = CertificateParams::new(Vec::new()).unwrap();
    admin_params
        .distinguished_name
        .push(DnType::CommonName, "admin");
    admin_params
        .distinguished_name
        .push(DnType::OrganizationName, "UnitctlTest");
    let admin_cert = admin_params
        .signed_by(&admin_key, &ca_cert, &ca_key)
        .unwrap();

    fs::write(certs_dir.join("admin.pem"), admin_cert.pem()).unwrap();
    fs::write(certs_dir.join("admin.key"), admin_key.serialize_pem()).unwrap();

    println!("Certificates generated in {}", certs_dir.display());
}
