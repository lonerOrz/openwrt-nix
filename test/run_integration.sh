#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONTAINER_NAME="openwrt-integration-test"
SSH_KEY_PATH="/tmp/openwrt_test_key"
SSH_CONFIG_PATH="/tmp/openwrt_test_ssh_config"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

pass() { echo -e "  ${GREEN}[PASS]${NC} $1"; }
fail() { echo -e "  ${RED}[FAIL]${NC} $1"; }
info() { echo -e "  ${YELLOW}[..]${NC} $1"; }
ok() { echo -e "  ${GREEN}[OK]${NC} $1"; }

cleanup() {
  echo ""
  echo "Cleaning up..."
  podman rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
  rm -f "$SSH_KEY_PATH" "$SSH_KEY_PATH.pub" "$SSH_CONFIG_PATH"
}
trap cleanup EXIT

# ── 1. Clean previous artifacts ──
echo "1/7 Cleaning previous artifacts"
podman rm -f "$CONTAINER_NAME" 2>/dev/null || true
rm -f "$SSH_KEY_PATH" "$SSH_KEY_PATH.pub" "$SSH_CONFIG_PATH"

# ── 2. Build and start container ──
echo "2/7 Building OpenWrt test container"
podman build -q -t openwrt-test-env -f "$PROJECT_ROOT/test/Containerfile" "$PROJECT_ROOT" >/dev/null
podman run -d --name "$CONTAINER_NAME" -p 2222:22 openwrt-test-env >/dev/null

# ── 3. Wait for dropbear ──
echo "3/7 Waiting for dropbear"
for i in {1..15}; do
  if (echo > /dev/tcp/127.0.0.1/2222) >/dev/null 2>&1; then
    ok "dropbear ready on port 2222"
    break
  fi
  if [ "$i" -eq 15 ]; then
    fail "dropbear startup timed out"
    podman logs "$CONTAINER_NAME" 2>&1 | tail -5
    exit 1
  fi
  sleep 1
done

# ── 4. Inject SSH key ──
echo "4/7 Injecting SSH key"
ssh-keygen -t ed25519 -N "" -f "$SSH_KEY_PATH" -C "openwrt-test" -q
podman exec -i "$CONTAINER_NAME" sh -c "mkdir -p /etc/dropbear && cat > /etc/dropbear/authorized_keys" < "$SSH_KEY_PATH.pub"
podman exec "$CONTAINER_NAME" chmod 700 /etc/dropbear
podman exec "$CONTAINER_NAME" chmod 600 /etc/dropbear/authorized_keys
ok "SSH key installed"

# ── 5. Create SSH config ──
echo "5/7 Creating SSH config"
cat <<EOF > "$SSH_CONFIG_PATH"
Host openwrt-test
    HostName localhost
    Port 2222
    User root
    StrictHostKeyChecking no
    UserKnownHostsFile /dev/null
    IdentityFile $SSH_KEY_PATH
EOF

# ── 6. Generate and deploy UCI commands ──
echo "6/7 Deploying UCI configuration"
GENERATED_CMDS=$(nix run "$PROJECT_ROOT#test-deploy" -- 2>/dev/null)
CMD_COUNT=$(echo "$GENERATED_CMDS" | wc -l)
info "Generated $CMD_COUNT UCI commands"

DEPLOY_STDERR=$(echo "$GENERATED_CMDS" | podman exec -i "$CONTAINER_NAME" sh -s 2>&1 >/dev/null)
UNEXPECTED_ERRORS=$(echo "$DEPLOY_STDERR" | grep -v "uci: Entry not found" | grep -v "^$" || true)
if [ -n "$UNEXPECTED_ERRORS" ]; then
  fail "Unexpected errors during deployment:"
  echo "$UNEXPECTED_ERRORS"
  exit 1
fi
ok "All commands executed successfully"

# ── 7. Verify UCI state ──
echo "7/7 Verifying UCI state"
FAILED=0

check_section() {
  if podman exec "$CONTAINER_NAME" uci get "$1" >/dev/null 2>&1; then
    ok "Section '$1' exists"
  else
    fail "Section '$1' not found"
    FAILED=1
  fi
}

check_value() {
  local actual
  actual=$(podman exec "$CONTAINER_NAME" uci get "$1")
  if [ "$actual" = "$2" ]; then
    pass "$3 = '$2'"
  else
    fail "$3 = '$actual', expected '$2'"
    FAILED=1
  fi
}

check_section "system.@system[0]"
check_section "wireless.default_radio0"
check_value "system.@system[0].hostname" "rauter" "hostname"
check_value "wireless.default_radio0.ssid" "gchq-2.4" "ssid"
check_value "wireless.default_radio0.key" "test-wifi-plain-password" "wifi key"
check_value "dropbear.@dropbear[0].PasswordAuth" "off" "dropbear PasswordAuth"

# ── Result ──
echo ""
if [ "$FAILED" -eq 0 ]; then
  echo -e "${GREEN}All integration tests passed${NC}"
else
  echo -e "${RED}Some integration tests FAILED${NC}"
  exit 1
fi
