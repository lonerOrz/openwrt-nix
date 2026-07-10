#!/usr/bin/env bash
# Test that the rollback watchdog actually restores config when SSH becomes unreachable.
# This is a standalone test — run separately from the main integration suite.
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONTAINER_NAME="openwrt-watchdog-test"
SSH_KEY_PATH="/tmp/openwrt_watchdog_key"
SSH_CONFIG_PATH="/tmp/openwrt_watchdog_ssh_config"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

pass() { echo -e "  ${GREEN}[PASS]${NC} $1"; }
fail() { echo -e "  ${RED}[FAIL]${NC} $1"; FAILURES=$((FAILURES + 1)); }
info() { echo -e "  ${YELLOW}[..]${NC} $1"; }

FAILURES=0

cleanup() {
  echo ""
  echo "Cleaning up..."
  podman rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
  rm -f "$SSH_KEY_PATH" "$SSH_KEY_PATH.pub" "$SSH_CONFIG_PATH"
}
trap cleanup EXIT

# ── 1. Build and start container ──
info "Building OpenWrt test container..."
podman rm -f "$CONTAINER_NAME" 2>/dev/null || true
podman build -q -t openwrt-test-env -f "$PROJECT_ROOT/test/Containerfile" "$PROJECT_ROOT" >/dev/null
podman run -d --name "$CONTAINER_NAME" -p 2223:22 openwrt-test-env >/dev/null

# ── 2. Wait for dropbear ──
info "Waiting for dropbear..."
for i in {1..15}; do
  if (echo > /dev/tcp/127.0.0.1/2223) >/dev/null 2>&1; then
    break
  fi
  if [ "$i" -eq 15 ]; then
    fail "dropbear startup timed out"
    exit 1
  fi
  sleep 1
done

# ── 3. Inject SSH key ──
info "Injecting SSH key..."
ssh-keygen -t ed25519 -N "" -f "$SSH_KEY_PATH" -C "openwrt-test" -q
podman exec -i "$CONTAINER_NAME" sh -c "mkdir -p /etc/dropbear && cat > /etc/dropbear/authorized_keys" < "$SSH_KEY_PATH.pub"
podman exec "$CONTAINER_NAME" chmod 700 /etc/dropbear
podman exec "$CONTAINER_NAME" chmod 600 /etc/dropbear/authorized_keys

cat <<EOF > "$SSH_CONFIG_PATH"
Host openwrt-test
    HostName localhost
    Port 2223
    User root
    StrictHostKeyChecking no
    UserKnownHostsFile /dev/null
    IdentityFile $SSH_KEY_PATH
EOF

# ── 4. Verify SSH works ──
info "Verifying SSH connectivity..."
if ssh -o BatchMode=yes -F "$SSH_CONFIG_PATH" openwrt-test "echo ok" 2>/dev/null | grep -q ok; then
  pass "Initial SSH connection works"
else
  fail "Initial SSH connection failed"
  exit 1
fi

# ── 5. Backup config and record state ──
info "Recording initial config state..."
INITIAL_HOSTNAME=$(ssh -F "$SSH_CONFIG_PATH" openwrt-test "uci get system.@system[0].hostname" 2>/dev/null || echo "unknown")
info "Initial hostname: $INITIAL_HOSTNAME"

ssh -F "$SSH_CONFIG_PATH" openwrt-test "cp -a /etc/config /tmp/.uci-rollback-backup"

# ── 6. Apply a breaking config (change SSH port to 9999) ──
info "Applying breaking config (changing dropbear port to 9999)..."
# Stop the current dropbear and restart on wrong port to simulate SSH loss
ssh -F "$SSH_CONFIG_PATH" openwrt-test "uci set dropbear.@dropbear[0].Port='9999' && uci commit && /etc/init.d/dropbear restart" 2>/dev/null || true

# ── 7. Start the watchdog process ──
info "Starting rollback watchdog (60s timeout)..."
ssh -F "$SSH_CONFIG_PATH" openwrt-test "( sleep 60; cp -a /tmp/.uci-rollback-backup/* /etc/config/; /etc/init.d/network restart; rm -rf /tmp/.uci-rollback-backup /tmp/.uci-watchdog-pid ) & echo \$! > /tmp/.uci-watchdog-pid" 2>/dev/null || true

# ── 8. Wait for SSH to become unreachable ──
info "Waiting for SSH to become unreachable..."
SSH_LOST=false
for i in $(seq 1 10); do
  sleep 1
  if ! ssh -o BatchMode=yes -o ConnectTimeout=2 -F "$SSH_CONFIG_PATH" openwrt-test "echo ok" 2>/dev/null | grep -q ok; then
    SSH_LOST=true
    break
  fi
done

if [ "$SSH_LOST" = true ]; then
  pass "SSH became unreachable after port change"
else
  fail "SSH remained reachable after port change (port change may not have taken effect)"
fi

# ── 9. Wait for watchdog to trigger (up to 70s) ──
info "Waiting for watchdog to restore config (up to 70s)..."
RESTORED=false
for i in $(seq 1 35); do
  sleep 2
  # Try to connect on port 22 (the original port after rollback)
  if ssh -o BatchMode=yes -o ConnectTimeout=2 -F "$SSH_CONFIG_PATH" openwrt-test "echo ok" 2>/dev/null | grep -q ok; then
    RESTORED=true
    break
  fi
done

if [ "$RESTORED" = true ]; then
  pass "SSH reconnected after watchdog rollback"
else
  fail "SSH did not reconnect within 70s — watchdog rollback may have failed"
fi

# ── 10. Verify config was rolled back ──
if [ "$RESTORED" = true ]; then
  info "Verifying config rollback..."
  # The dropbear port should be back to 22 (from the backup)
  CURRENT_PORT=$(ssh -F "$SSH_CONFIG_PATH" openwrt-test "uci get dropbear.@dropbear[0].Port" 2>/dev/null || echo "unknown")
  if [ "$CURRENT_PORT" = "22" ]; then
    pass "Dropbear port rolled back to 22"
  else
    fail "Dropbear port is '$CURRENT_PORT', expected '22'"
  fi

  # Verify watchdog PID file was cleaned up
  WATCHDOG_PID_EXISTS=$(ssh -F "$SSH_CONFIG_PATH" openwrt-test "test -f /tmp/.uci-watchdog-pid && echo yes || echo no" 2>/dev/null || echo "unknown")
  if [ "$WATCHDOG_PID_EXISTS" = "no" ]; then
    pass "Watchdog PID file cleaned up"
  else
    fail "Watchdog PID file still exists"
  fi

  # Verify rollback backup was cleaned up
  BACKUP_EXISTS=$(ssh -F "$SSH_CONFIG_PATH" openwrt-test "test -d /tmp/.uci-rollback-backup && echo yes || echo no" 2>/dev/null || echo "unknown")
  if [ "$BACKUP_EXISTS" = "no" ]; then
    pass "Rollback backup directory cleaned up"
  else
    fail "Rollback backup directory still exists"
  fi
fi

# ── Result ──
echo ""
if [ "$FAILURES" -eq 0 ]; then
  echo -e "${GREEN}All watchdog rollback tests passed${NC}"
else
  echo -e "${RED}$FAILURES watchdog rollback test(s) FAILED${NC}"
  exit 1
fi
