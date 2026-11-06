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
ssh -p "${PORT}" ${SSH_OPTS} "root@${HOST}" "sysupgrade -v /tmp/openwrt.sysupgrade.itb" || true # device reboots and drops SSH, so the command exits nonzero on success

while ! ping -c1 -W1 8.8.8.8; do sleep 2; done
