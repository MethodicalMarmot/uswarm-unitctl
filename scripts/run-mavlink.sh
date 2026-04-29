#!/bin/bash

[ -z "${GCS_IP}" ] && { echo "GCS_IP is not set" ; exit 1 ; }
[ -z "${REMOTE_MAVLINK_PORT}" ] && { echo "REMOTE_MAVLINK_PORT is not set"; exit 1; }
[ -z "${SNIFFER_SYS_ID}" ] && { echo "SNIFFER_SYS_ID is not set"; exit 1; }
[ -z "${LOCAL_MAVLINK_PORT}" ] && { echo "LOCAL_MAVLINK_PORT is not set"; exit 1; }
[ -z "${FC_TTY}" ] && { echo "FC_TTY is not set"; exit 1; }
[ -z "${FC_BAUDRATE}" ] && { echo "FC_BAUDRATE is not set"; exit 1; }

if ! [[ "${GCS_IP}" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  RESOLVED_IP=$(dig +short "${GCS_IP}" 2>/dev/null)
  [ -z "${RESOLVED_IP}" ] && { echo "Failed to resolve GCS_IP: ${GCS_IP}"; exit 1; }
  echo "Resolved GCS_IP ${GCS_IP} -> ${RESOLVED_IP}"
  GCS_IP="${RESOLVED_IP}"
fi

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )
pushd "$SCRIPT_DIR/.."

./mavlink-routerd/mavlink-routerd-glibc-$(uname -m) \
  -e ${GCS_IP}:${REMOTE_MAVLINK_PORT} \
  -s ${SNIFFER_SYS_ID} \
  -t ${LOCAL_MAVLINK_PORT} \
  ${FC_TTY}:${FC_BAUDRATE}
