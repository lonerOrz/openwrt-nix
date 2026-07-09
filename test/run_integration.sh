#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONTAINER_NAME="openwrt-integration-test"
SSH_KEY_PATH="/tmp/openwrt_test_key"
SSH_CONFIG_PATH="/tmp/openwrt_test_ssh_config"

cleanup() {
  echo "=== Cleaning up ==="
  podman rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
  rm -f "$SSH_KEY_PATH" "$SSH_KEY_PATH.pub" "$SSH_CONFIG_PATH"
}
trap cleanup EXIT

echo "=== 1. Cleaning previous test artifacts ==="
podman rm -f "$CONTAINER_NAME" 2>/dev/null || true
rm -f "$SSH_KEY_PATH" "$SSH_KEY_PATH.pub" "$SSH_CONFIG_PATH"

echo "=== 2. Building and running OpenWrt test container ==="
podman build -t openwrt-test-env -f "$PROJECT_ROOT/test/Containerfile" "$PROJECT_ROOT"
podman run -d \
  --name "$CONTAINER_NAME" \
  -p 2222:22 \
  openwrt-test-env

echo "=== 3. Waiting for Dropbear to be ready ==="
for i in {1..15}; do
  if (echo > /dev/tcp/127.0.0.1/2222) >/dev/null 2>&1; then
    echo "  [OK] Port 2222 is open"
    break
  fi
  if [ "$i" -eq 15 ]; then
    echo "  [ERROR] Dropbear startup timed out"
    podman logs "$CONTAINER_NAME"
    exit 1
  fi
  sleep 1
done

echo "=== 4. Generating temporary SSH credentials ==="
ssh-keygen -t ed25519 -N "" -f "$SSH_KEY_PATH" -C "openwrt-test"
podman exec -i "$CONTAINER_NAME" sh -c "mkdir -p /etc/dropbear && cat > /etc/dropbear/authorized_keys" < "$SSH_KEY_PATH.pub"
podman exec "$CONTAINER_NAME" chmod 700 /etc/dropbear
podman exec "$CONTAINER_NAME" chmod 600 /etc/dropbear/authorized_keys

echo "=== 5. Creating temporary SSH config ==="
cat <<EOF > "$SSH_CONFIG_PATH"
Host openwrt-test
    HostName localhost
    Port 2222
    User root
    StrictHostKeyChecking no
    UserKnownHostsFile /dev/null
    IdentityFile $SSH_KEY_PATH
EOF

echo "=== 6. Deploying UCI configuration to container ==="
export SSH_OPTS="-F $SSH_CONFIG_PATH"
# Generate UCI commands locally, then pipe to container via SSH
nix run "$PROJECT_ROOT#test-deploy" -- 2>/dev/null | ssh -F "$SSH_CONFIG_PATH" openwrt-test 'sh -s'

echo "=== 7. Running assertions ==="
FAILED=0

ACTUAL_HOSTNAME=$(podman exec "$CONTAINER_NAME" uci get system.@system[0].hostname)
if [ "$ACTUAL_HOSTNAME" = "rauter" ]; then
  echo "  [PASS] hostname = 'rauter'"
else
  echo "  [FAIL] hostname = '$ACTUAL_HOSTNAME', expected 'rauter'"
  FAILED=1
fi

ACTUAL_SSID=$(podman exec "$CONTAINER_NAME" uci get wireless.default_radio0.ssid)
if [ "$ACTUAL_SSID" = "gchq-2.4" ]; then
  echo "  [PASS] ssid = 'gchq-2.4'"
else
  echo "  [FAIL] ssid = '$ACTUAL_SSID', expected 'gchq-2.4'"
  FAILED=1
fi

ACTUAL_KEY=$(podman exec "$CONTAINER_NAME" uci get wireless.default_radio0.key)
if [ "$ACTUAL_KEY" = "test-wifi-plain-password" ]; then
  echo "  [PASS] wifi key = 'test-wifi-plain-password'"
else
  echo "  [FAIL] wifi key = '$ACTUAL_KEY', expected 'test-wifi-plain-password'"
  FAILED=1
fi

if [ "$FAILED" -eq 0 ]; then
  echo "=== All integration tests passed ==="
else
  echo "=== Some integration tests FAILED ==="
  exit 1
fi
