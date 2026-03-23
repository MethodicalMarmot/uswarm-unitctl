#!/bin/bash
# Run MQTT integration tests with a Mosquitto broker in Docker.
#
# Usage: ./tests/run_mqtt_tests.sh [extra cargo test args...]
#
# Examples:
#   ./tests/run_mqtt_tests.sh                           # run all MQTT integration tests
#   ./tests/run_mqtt_tests.sh test_tls_connection       # run a specific test
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
COMPOSE_FILE="$SCRIPT_DIR/docker-compose.mqtt.yml"

cleanup() {
    echo "Stopping Mosquitto broker..."
    docker compose -f "$COMPOSE_FILE" down --timeout 5 2>/dev/null || true
}
trap cleanup EXIT

echo "Starting Mosquitto broker..."
docker compose -f "$COMPOSE_FILE" up -d --wait

echo "Running MQTT integration tests..."
cargo test --test mqtt_integration -- --ignored "$@"
