#!/usr/bin/bash -eu
set -o pipefail

systemctl-exists() {
  [ $(systemctl list-unit-files "${1}*" | wc -l) -gt 3 ]
}

maybe_start() {
  if [ "$SKIP_START" = false ]; then
    systemctl start "$@"
  else
    echo "Skipping start of $*"
  fi
}

install_packages() {
  apt-get update
  apt-get install -y --no-install-recommends \
        systemd systemd-sysv iputils-ping \
        socat bash ca-certificates curl gnupg \
        gstreamer1.0-tools gstreamer1.0-plugins-base \
        gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
        gstreamer1.0-plugins-ugly gstreamer1.0-libav \
        gstreamer1.0-x \
        libssl3 libdbus-1-3 modemmanager dnsutils rsync

  # Fluent Bit official apt repository (Debian bookworm).
  install -d -m 0755 /usr/share/keyrings
  curl -fsSL https://packages.fluentbit.io/fluentbit.key \
    | gpg --dearmor --yes -o /usr/share/keyrings/fluentbit-keyring.gpg
  echo "deb [signed-by=/usr/share/keyrings/fluentbit-keyring.gpg] https://packages.fluentbit.io/debian/bookworm bookworm main" \
    > /etc/apt/sources.list.d/fluent-bit.list
  apt-get update
  apt-get install -y --no-install-recommends fluent-bit

  rm -rf /var/lib/apt/lists/*
}

install() {
  echo "Starting installation..."

  rsync -av --itemize-changes ./assets/_ops/etc/udev/rules.d /etc/udev | grep '^>' && {
    udevadm control --reload-rules
    udevadm trigger
  }

  echo "Setting up mavlink services..."
  systemctl-exists mavlink.service || systemctl link ./services/mavlink.service
  systemctl-exists mavlink-watcher.service || {
    systemctl link ./services/mavlink-watcher.service
    systemctl enable ./services/mavlink-watcher.path
    maybe_start mavlink-watcher.path
  }
  systemctl-exists mavlink-restart.service || systemctl link ./services/mavlink-restart.service
  systemctl-exists mavlink-restart.timer || {
    systemctl link ./services/mavlink-restart.timer
    systemctl enable ./services/mavlink-restart.timer
    maybe_start mavlink-restart.timer
  }

  echo "Setting up camera services..."
  systemctl-exists camera.service || systemctl link ./services/camera.service
  systemctl-exists camera-watcher.service || {
    systemctl link ./services/camera-watcher.service
    systemctl enable ./services/camera-watcher.path
    maybe_start camera-watcher.path
  }

  echo "Setting up fluentbit services..."
  systemctl-exists fluentbit.service || systemctl link ./services/fluentbit.service
  systemctl-exists fluentbit-watcher.service || {
    systemctl link ./services/fluentbit-watcher.service
    systemctl enable ./services/fluentbit-watcher.path
    maybe_start fluentbit-watcher.path
  }

  systemctl-exists unitctl.service || systemctl enable ./services/unitctl.service
  systemctl-exists unitctl-watcher.service || {
    systemctl link ./services/unitctl-watcher.service
    systemctl enable ./services/unitctl-watcher.path
    maybe_start unitctl-watcher.path
  }

  systemctl-exists modem-restart.service || systemctl link ./services/modem-restart.service
  if [ "$SKIP_DEVICE_WATCHDOG" = false ]; then
    systemctl-exists modem-restart.timer || {
      systemctl link ./services/modem-restart.timer
      systemctl enable ./services/modem-restart.timer
      maybe_start modem-restart.timer
    }

    echo "Setting up tty-fc-check service..."
    systemctl-exists tty-fc-check.service || systemctl enable ./services/tty-fc-check.service
  fi
}

uninstall() {
    echo "Uninstalling services..."
    systemctl disable --now mavlink-watcher.path mavlink-restart.timer camera-watcher.path unitctl-watcher.path modem-restart.timer fluentbit-watcher.path || true
    systemctl disable --now mavlink.service || true
    systemctl disable --now mavlink-watcher.service || true
    systemctl disable --now camera.service || true
    systemctl disable --now camera-watcher.service || true
    systemctl disable --now fluentbit.service || true
    systemctl disable --now fluentbit-watcher.service || true
    systemctl disable --now unitctl.service || true
    systemctl disable --now unitctl-watcher.service || true
    systemctl disable --now modem-restart.service || true
    systemctl disable --now tty-fc-check.service || true

    echo "Uninstallation complete. Note that any files created by the services will not be removed."
}

SKIP_DEVICE_WATCHDOG=false
SKIP_START=false

usage() {
  local exit_code=${1:-1}
  cat <<EOF
Usage: $0 {install|uninstall} [options]

Subcommands:
  install      Install and start all services managed by this script. This is idempotent and can be safely re-run.
  uninstall    Uninstall all services managed by this script. This will not remove any files created by the services.

Options:
  -h, --help
      Show this help message and exit.

  --skip-device-watchdog
      Skip installation of modem and FC device watchdog services. This is useful for development and debugging, but not recommended for production use.

  --skip-start
      Enable systemd services/timers/paths but do not start them. Useful when building images or deferring startup to the next boot.
EOF
  exit "$exit_code"
}

if [ "$#" -lt 1 ]; then
  usage
fi

case "$1" in
  -h|--help)
    usage 0
    ;;
esac


COMMAND=$1
shift

while [ "$#" -gt 0 ]; do
  case "$1" in
    -h|--help)
      usage 0
      ;;
    --skip-device-watchdog)
      SKIP_DEVICE_WATCHDOG=true
      ;;
    --skip-start)
      SKIP_START=true
      ;;
    *)
      echo "Unknown option: $1"
      usage
      ;;
  esac
  shift
done

# Change to script's directory for all operations
pushd $( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )/.. > /dev/null

# check if running as root (needed for armbian)
if [ "$(id -u)" -ne 0 ]; then
  echo "This script must be run as root or sudo."
  popd > /dev/null
  exit 1
fi

case "$COMMAND" in
  install)
    install_packages
    install
    ;;
  uninstall)
    uninstall
    ;;
  *)
    echo "Unknown command: $COMMAND"
    usage
    ;;
esac

popd > /dev/null