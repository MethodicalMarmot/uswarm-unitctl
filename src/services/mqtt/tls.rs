use std::io;

use rumqttc::TlsConfiguration;
use x509_parser::pem::parse_x509_pem;

/// Errors that can occur during TLS certificate operations.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("certificate parse error: {0}")]
    CertParse(String),
    #[error("no CN found in certificate subject")]
    NoCommonName,
}

/// Load TLS configuration for mutual TLS authentication with an MQTT broker.
///
/// Reads CA certificate, client certificate, and client private key from the
/// given file paths and returns a `rumqttc::TlsConfiguration` ready for use
/// with the MQTT client.
pub fn load_tls_config(
    ca_path: &str,
    cert_path: &str,
    key_path: &str,
) -> Result<TlsConfiguration, TlsError> {
    let ca = std::fs::read(ca_path)?;
    let client_cert = std::fs::read(cert_path)?;
    let client_key = std::fs::read(key_path)?;

    Ok(TlsConfiguration::Simple {
        ca,
        alpn: None,
        client_auth: Some((client_cert, client_key)),
    })
}

/// Extract the node ID from a client certificate's Common Name (CN) field.
///
/// Reads a PEM-encoded X.509 certificate from `cert_path` and returns the
/// value of the first CN attribute found in the certificate's subject.
pub fn extract_node_id(cert_path: &str) -> Result<String, TlsError> {
    let pem_data = std::fs::read(cert_path)?;
    extract_node_id_from_pem(&pem_data)
}

/// Extract CN from PEM-encoded certificate bytes (testable without file I/O).
fn extract_node_id_from_pem(pem_data: &[u8]) -> Result<String, TlsError> {
    let (_, pem) = parse_x509_pem(pem_data).map_err(|e| TlsError::CertParse(format!("{e}")))?;
    let cert = pem
        .parse_x509()
        .map_err(|e| TlsError::CertParse(format!("{e}")))?;

    for rdn in cert.subject().iter() {
        for attr in rdn.iter() {
            if attr.attr_type() == &x509_parser::oid_registry::OID_X509_COMMON_NAME {
                let cn = attr
                    .as_str()
                    .map_err(|e| TlsError::CertParse(format!("CN is not valid UTF-8: {e}")))?;
                return Ok(cn.to_string());
            }
        }
    }

    Err(TlsError::NoCommonName)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Generate a self-signed certificate with the given CN using rcgen.
    fn generate_test_cert(cn: &str) -> (tempfile::NamedTempFile, tempfile::NamedTempFile) {
        let mut params = rcgen::CertificateParams::new(Vec::new()).unwrap();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, cn);

        let key_pair = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key_pair).unwrap();

        let mut cert_file = tempfile::NamedTempFile::new().unwrap();
        cert_file.write_all(cert.pem().as_bytes()).unwrap();

        let mut key_file = tempfile::NamedTempFile::new().unwrap();
        key_file
            .write_all(key_pair.serialize_pem().as_bytes())
            .unwrap();

        (cert_file, key_file)
    }

    /// Generate a certificate with Organization only (no CN).
    fn generate_test_cert_no_cn() -> tempfile::NamedTempFile {
        let mut params = rcgen::CertificateParams::new(Vec::new()).unwrap();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::OrganizationName, "TestOrg");

        let cert = params
            .self_signed(&rcgen::KeyPair::generate().unwrap())
            .unwrap();

        let mut cert_file = tempfile::NamedTempFile::new().unwrap();
        cert_file.write_all(cert.pem().as_bytes()).unwrap();
        cert_file
    }

    #[test]
    fn test_extract_node_id_from_cert() {
        let (cert_file, _key_file) = generate_test_cert("drone-unit-42");
        let node_id = extract_node_id(cert_file.path().to_str().unwrap()).unwrap();
        assert_eq!(node_id, "drone-unit-42");
    }

    #[test]
    fn test_extract_node_id_complex_cn() {
        let (cert_file, _key_file) = generate_test_cert("node-abc-123-def");
        let node_id = extract_node_id(cert_file.path().to_str().unwrap()).unwrap();
        assert_eq!(node_id, "node-abc-123-def");
    }

    #[test]
    fn test_extract_node_id_no_cn() {
        let cert_file = generate_test_cert_no_cn();
        let result = extract_node_id(cert_file.path().to_str().unwrap());
        assert!(matches!(result, Err(TlsError::NoCommonName)));
    }

    #[test]
    fn test_extract_node_id_invalid_pem() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"not a valid PEM certificate").unwrap();

        let result = extract_node_id(file.path().to_str().unwrap());
        assert!(matches!(result, Err(TlsError::CertParse(_))));
    }

    #[test]
    fn test_extract_node_id_file_not_found() {
        let result = extract_node_id("/nonexistent/path/cert.pem");
        assert!(matches!(result, Err(TlsError::Io(_))));
    }

    #[test]
    fn test_load_tls_config_valid() {
        let (cert_file, key_file) = generate_test_cert("test-node");
        let ca_path = cert_file.path().to_str().unwrap();
        let cert_path = cert_file.path().to_str().unwrap();
        let key_path = key_file.path().to_str().unwrap();

        let config = load_tls_config(ca_path, cert_path, key_path);
        assert!(config.is_ok());

        match config.unwrap() {
            TlsConfiguration::Simple {
                ca,
                alpn,
                client_auth,
            } => {
                assert!(!ca.is_empty());
                assert!(alpn.is_none());
                let (cert, key) = client_auth.unwrap();
                assert!(!cert.is_empty());
                assert!(!key.is_empty());
            }
            _ => panic!("expected TlsConfiguration::Simple"),
        }
    }

    #[test]
    fn test_load_tls_config_missing_ca() {
        let (cert_file, key_file) = generate_test_cert("test-node");
        let result = load_tls_config(
            "/nonexistent/ca.pem",
            cert_file.path().to_str().unwrap(),
            key_file.path().to_str().unwrap(),
        );
        assert!(matches!(result, Err(TlsError::Io(_))));
    }

    #[test]
    fn test_load_tls_config_missing_cert() {
        let (cert_file, key_file) = generate_test_cert("test-node");
        let result = load_tls_config(
            cert_file.path().to_str().unwrap(),
            "/nonexistent/cert.pem",
            key_file.path().to_str().unwrap(),
        );
        assert!(matches!(result, Err(TlsError::Io(_))));
    }

    #[test]
    fn test_load_tls_config_missing_key() {
        let (cert_file, _key_file) = generate_test_cert("test-node");
        let result = load_tls_config(
            cert_file.path().to_str().unwrap(),
            cert_file.path().to_str().unwrap(),
            "/nonexistent/key.pem",
        );
        assert!(matches!(result, Err(TlsError::Io(_))));
    }
}
