#!/usr/bin/env bash
set -eu

# Usage: reset-modem.sh <interface>
# Example: reset-modem.sh wwan0

if [ "$#" -lt 1 ]; then
  echo "Usage: $0 <interface>"
  exit 2
fi

IFACE="$1"
MODEM_DEV="/dev/ttyModem"

if [ ! -e "$MODEM_DEV" ]; then
  echo "$MODEM_DEV does not exist. Exiting."
  exit 1
fi

eval $(udevadm info -q property -n $MODEM_DEV | grep -E 'ID_VENDOR_ID|ID_MODEL_ID')
/usr/bin/usbreset | grep $ID_VENDOR_ID | grep $ID_MODEL_ID >/dev/null || {
  echo "No modem found with VID:PID ${ID_VENDOR_ID}:${ID_MODEL_ID}"
  exit 1
}

CMD="/usr/bin/usbreset ${ID_VENDOR_ID}:${ID_MODEL_ID}"

# Where to store lock/timestamp. Falls back to /tmp if /var/run not writable.
BASE_DIR="/var/run"
if [ ! -w "${BASE_DIR}" ]; then
  BASE_DIR="/tmp"
fi

LOCKFILE="${BASE_DIR}/reset-modem.${IFACE}.lock"
TSFILE="${BASE_DIR}/reset-modem.${IFACE}.ts"
MIN_INTERVAL=120   # seconds to wait between runs

# Acquire an exclusive lock on the lockfile (prevents races when run concurrently)
exec 9>"${LOCKFILE}"
if ! flock -n 9; then
  # another instance is running; exit quietly
  echo "Another instance is running for ${IFACE}. Exiting."
  exit 0
fi

# function to clean up lock descriptor on exit
cleanup() {
  exec 9>&-
}
trap cleanup EXIT

# Check if the interface has any IPv4 or IPv6 address assigned.
# If you only care about IPv4, change the grep to 'inet ' (not 'inet6').
if ip addr show dev "${IFACE}" 2>/dev/null | grep -qE 'inet |inet6 '; then
  echo "Interface ${IFACE} has an IP address. Nothing to do."
  exit 0
fi

# If timestamp file exists, check elapsed time since last run
now=$(date +%s)
if [ -f "${TSFILE}" ]; then
  last=$(cat "${TSFILE}" 2>/dev/null || echo 0)
  # ensure last is numeric
  if ! [[ "${last}" =~ ^[0-9]+$ ]]; then
    last=0
  fi
  elapsed=$(( now - last ))
  if [ "${elapsed}" -lt "${MIN_INTERVAL}" ]; then
    echo "Last run was ${elapsed}s ago (< ${MIN_INTERVAL}s). Skipping."
    exit 0
  fi
fi

# Run the command. If it fails, exit with its exit code but still update timestamp.
echo "No IP on ${IFACE}. Running: $CMD"
bash -c "$CMD"
rc=$?

# Record timestamp of this run (even if command failed) to avoid immediate retries.
printf '%s\n' "${now}" > "${TSFILE}"

exit "${rc}"

