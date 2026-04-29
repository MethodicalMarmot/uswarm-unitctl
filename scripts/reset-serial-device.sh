#!/bin/bash
DEV_PATHS=(
    "/dev/ttyFC"
    "/dev/ttyFC1"
)

USB_IDS="1a86:7523|1a86:55d2"
USBRESET="/usr/bin/usbreset"

MAX_ATTEMPTS=12
SLEEP_INTERVAL=5

devices_exist() {
    for dev in "${DEV_PATHS[@]}"; do
        if [ ! -e "$dev" ]; then
            return 1
        fi
    done

    echo "All devices found: ${DEV_PATHS[*]}"
    return 0
}

attempt=1
while ! devices_exist; do
    echo "[$(date)] Some required devices are not found. Attempt $attempt/$MAX_ATTEMPTS: resetting matching USB devices..."

    $USBRESET | grep -E "$USB_IDS" | while read -r line; do
        BUSDDEV=$(echo "$line" | awk '{print $2}')
        USB_ID=$(echo "$line" | awk '{print $4}')
        if [[ -n "$BUSDDEV" ]]; then
            echo "Resetting USB device $BUSDDEV (ID $USB_ID)..."
            $USBRESET "$BUSDDEV"
        fi
    done
    ((attempt++))
    if (( attempt > MAX_ATTEMPTS )); then
        echo "Timeout reached. Devices still not available."
        exit 1
    fi
    sleep "$SLEEP_INTERVAL"
done
echo "Device available. Exiting successfully."
exit 0