#!/usr/bin/env bash
set -euo pipefail

HOST="$1"
PORT="$2"
SSH_OPTS="${3:-}"

version="$(curl --silent https://api.github.com/repos/openwrt/openwrt/releases/latest | jq -r .tag_name | sed 's/^v//')"
sysupgrade_url="https://downloads.openwrt.org/releases/${version}/targets/mediatek/mt7622/openwrt-${version}-mediatek-mt7622-linksys_e8450-ubi-squashfs-sysupgrade.itb"

echo "Downloading OpenWrt ${version}..."
wget "${sysupgrade_url}" -O openwrt.sysupgrade.itb
ssh -p "${PORT}" ${SSH_OPTS} "root@${HOST}" "cat > /tmp/openwrt.sysupgrade.itb" <openwrt.sysupgrade.itb
ssh -p "${PORT}" ${SSH_OPTS} "root@${HOST}" "sysupgrade -v /tmp/openwrt.sysupgrade.itb"
# sysupgrade reboots the device, so the SSH command above is *expected* to exit
# non-zero (the connection drops mid-reboot). That is the success signal, not a
# failure — do not let set -e treat it as one. We keep going to wait for the box
# to come back, and the wait loop below enforces reachability before returning.

MAX_TRIES=60
try=0
while ! ping -c1 -W1 "${HOST}" >/dev/null 2>&1; do
  try=$((try + 1))
  if [ "${try}" -ge "${MAX_TRIES}" ]; then
    echo "ERROR: ${HOST} did not come back after ${MAX_TRIES} pings" >&2
    exit 1
  fi
  sleep 2
done
