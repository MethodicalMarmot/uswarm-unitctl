#!/bin/bash
# Generate test certificates for MQTT mTLS integration tests.
# Creates: CA cert/key, server cert/key, client cert/key (CN=test-drone-01),
#          admin cert/key (CN=admin, full topic access)
set -euo pipefail

CERTS_DIR="$(cd "$(dirname "$0")/certs" && pwd)"
DAYS=3650

echo "Generating test certificates in $CERTS_DIR ..."

# CA key and self-signed certificate
openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$CERTS_DIR/ca.key" \
    -out "$CERTS_DIR/ca.pem" \
    -days $DAYS \
    -subj "/CN=Test MQTT CA/O=UnitctlTest"

# Server key and CSR
openssl req -newkey rsa:2048 -nodes \
    -keyout "$CERTS_DIR/server.key" \
    -out "$CERTS_DIR/server.csr" \
    -subj "/CN=localhost/O=UnitctlTest"

# Sign server cert with CA
openssl x509 -req \
    -in "$CERTS_DIR/server.csr" \
    -CA "$CERTS_DIR/ca.pem" \
    -CAkey "$CERTS_DIR/ca.key" \
    -CAcreateserial \
    -out "$CERTS_DIR/server.pem" \
    -days $DAYS \
    -extfile <(printf "subjectAltName=DNS:localhost,IP:127.0.0.1,IP:192.168.254.128")

# Client key and CSR (CN = node ID)
openssl req -newkey rsa:2048 -nodes \
    -keyout "$CERTS_DIR/client.key" \
    -out "$CERTS_DIR/client.csr" \
    -subj "/CN=test-drone-01/O=UnitctlTest"

# Sign client cert with CA
openssl x509 -req \
    -in "$CERTS_DIR/client.csr" \
    -CA "$CERTS_DIR/ca.pem" \
    -CAkey "$CERTS_DIR/ca.key" \
    -CAcreateserial \
    -out "$CERTS_DIR/client.pem" \
    -days $DAYS

# Admin key and CSR (CN = admin, full topic access)
openssl req -newkey rsa:2048 -nodes \
    -keyout "$CERTS_DIR/admin.key" \
    -out "$CERTS_DIR/admin.csr" \
    -subj "/CN=admin/O=UnitctlTest"

# Sign admin cert with CA
openssl x509 -req \
    -in "$CERTS_DIR/admin.csr" \
    -CA "$CERTS_DIR/ca.pem" \
    -CAkey "$CERTS_DIR/ca.key" \
    -CAcreateserial \
    -out "$CERTS_DIR/admin.pem" \
    -days $DAYS

# Clean up CSRs and serial
rm -f "$CERTS_DIR"/*.csr "$CERTS_DIR"/*.srl

echo "Done. Files:"
ls -la "$CERTS_DIR"
