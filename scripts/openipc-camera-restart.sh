#!/bin/bash

camera_ip=""

while [ -z "$camera_ip" ]; do
  camera_ip=$(sudo cat /var/lib/NetworkManager/dnsmasq-eth0.leases | grep openipc | cut -d' ' -f3)
  if [ -z "$camera_ip" ]; then
    echo "Waiting for camera..."
    sleep 1
  fi
done

echo "Camera ip: $camera_ip"

while true; do
  ping -c1 -W1 $camera_ip && break
  echo "Camera is offline, waiting..."
  sleep 1
done

echo "Camera is online. Rebooting"

sshpass -p 12345 ssh -o 'StrictHostKeyChecking=no' root@$camera_ip 'reboot'

sleep 5

while true; do
  nmap -p 80 $camera_ip | grep open && break
  echo "Waiting for camera back online..."
  sleep 1
done

type -t http > /dev/null && http http://$camera_ip/night/ircut