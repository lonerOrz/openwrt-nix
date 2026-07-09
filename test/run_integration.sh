#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONTAINER_NAME="openwrt-integration-test"
SSH_KEY_PATH="/tmp/openwrt_test_key"
SSH_CONFIG_PATH="/tmp/openwrt_test_ssh_config"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
NC='\033[0m'

pass() { echo -e "  ${GREEN}[PASS]${NC} $1"; }
fail() { echo -e "  ${RED}[FAIL]${NC} $1"; FAILURES=$((FAILURES + 1)); }
info() { echo -e "  ${YELLOW}[..]${NC} $1"; }
ok() { echo -e "  ${GREEN}[OK]${NC} $1"; }
section() { echo -e "\n${CYAN}$1${NC}"; }

FAILURES=0

cleanup() {
  echo ""
  echo "Cleaning up..."
  podman rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
  rm -f "$SSH_KEY_PATH" "$SSH_KEY_PATH.pub" "$SSH_CONFIG_PATH"
}
trap cleanup EXIT

# ── 1. Clean previous artifacts ──
section "1/9 Cleaning previous artifacts"
podman rm -f "$CONTAINER_NAME" 2>/dev/null || true
rm -f "$SSH_KEY_PATH" "$SSH_KEY_PATH.pub" "$SSH_CONFIG_PATH"

# ── 2. Build and start container ──
section "2/9 Building OpenWrt test container"
podman build -q -t openwrt-test-env -f "$PROJECT_ROOT/test/Containerfile" "$PROJECT_ROOT" >/dev/null
podman run -d --name "$CONTAINER_NAME" -p 2222:22 openwrt-test-env >/dev/null

# ── 3. Wait for dropbear ──
section "3/9 Waiting for dropbear"
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
section "4/9 Injecting SSH key"
ssh-keygen -t ed25519 -N "" -f "$SSH_KEY_PATH" -C "openwrt-test" -q
podman exec -i "$CONTAINER_NAME" sh -c "mkdir -p /etc/dropbear && cat > /etc/dropbear/authorized_keys" < "$SSH_KEY_PATH.pub"
podman exec "$CONTAINER_NAME" chmod 700 /etc/dropbear
podman exec "$CONTAINER_NAME" chmod 600 /etc/dropbear/authorized_keys
ok "SSH key installed"

# ── 5. Create SSH config ──
section "5/9 Creating SSH config"
cat <<EOF > "$SSH_CONFIG_PATH"
Host openwrt-test
    HostName localhost
    Port 2222
    User root
    StrictHostKeyChecking no
    UserKnownHostsFile /dev/null
    IdentityFile $SSH_KEY_PATH
EOF

# ── 6. Verify nuci command generation ──
section "6/9 Verifying nuci command generation"
NUCI_OUTPUT=$(nix run "$PROJECT_ROOT#test-deploy" -- 2>/dev/null)

check_cmd() {
  if echo "$NUCI_OUTPUT" | grep -qF "$1"; then
    pass "$2"
  else
    fail "$2 — pattern not found: $1"
  fi
}

check_cmd "uci add system system" "list section: system created via add"
# Verify redundant type set was removed (uci add already sets type)
if echo "$NUCI_OUTPUT" | grep -qF "uci set system.@system[0]=system"; then
  fail "Redundant type set still present for list sections"
else
  pass "Redundant type set correctly removed"
fi
check_cmd "uci set system.@system[0].hostname='rauter'" "list section: hostname set"
check_cmd "uci set system.@system[0].timezone='UTC'" "list section: timezone set"
check_cmd "uci delete wireless.default_radio0" "named section: wireless deleted before recreate"
check_cmd "uci set wireless.default_radio0=wifi-iface" "named section: wireless type set"
check_cmd "uci set wireless.default_radio0.ssid='gchq-2.4'" "named section: ssid set"
check_cmd "uci set wireless.default_radio0.key='test-wifi-plain-password'" "named section: wifi key set"
check_cmd "uci delete network.lan" "named section: network deleted before recreate"
check_cmd "uci set network.lan=interface" "named section: network type set"
check_cmd "uci set network.lan.proto='static'" "named section: lan proto set"
check_cmd "uci set network.lan.ipaddr='192.168.1.1'" "named section: lan ipaddr set"
check_cmd "uci commit" "output: uci commit present"
check_cmd "printf '' > /etc/opkg/customfeeds.conf" "opkg: feeds file created"
check_cmd "src/gz custom https://example.com/packages" "opkg: feed entry correct"
check_cmd "opkg update && opkg install luci tcpdump" "opkg: packages install command"
check_cmd "opkg install /tmp/test-package_1.0_all.ipk" "opkg: local package install"

# ── 7. Deploy to container and verify state ──
section "7/9 Deploying full script and verifying container state"
# Validate full script syntax before execution
SYNTAX_ERR=$(echo "$NUCI_OUTPUT" | podman exec -i "$CONTAINER_NAME" sh -n 2>&1)
if [ -n "$SYNTAX_ERR" ]; then
  fail "Syntax error in deployment script: $SYNTAX_ERR"
else
  ok "Full deployment script passes sh -n syntax check"
fi

# Deploy full script (UCI + opkg + feeds) — mock opkg handles package commands
DEPLOY_STDERR=$(echo "$NUCI_OUTPUT" | podman exec -i "$CONTAINER_NAME" sh -s 2>&1 >/dev/null || true)
UNEXPECTED_ERRORS=$(echo "$DEPLOY_STDERR" | grep -v "uci: Entry not found" | grep -v "^$" || true)
if [ -n "$UNEXPECTED_ERRORS" ]; then
  fail "Unexpected errors during deployment:"
  echo "$UNEXPECTED_ERRORS"
else
  ok "All commands executed without errors"
fi

check_value() {
  local actual
  actual=$(podman exec "$CONTAINER_NAME" uci get "$1" 2>/dev/null)
  if [ "$actual" = "$2" ]; then
    pass "$3 = '$2'"
  else
    fail "$3 = '$actual', expected '$2'"
  fi
}

check_section() {
  if podman exec "$CONTAINER_NAME" uci get "$1" >/dev/null 2>&1; then
    ok "Section '$1' exists"
  else
    fail "Section '$1' not found"
  fi
}

check_section "system.@system[0]"
check_section "wireless.default_radio0"
check_section "network.lan"

check_value "system.@system[0].hostname" "rauter" "hostname"
check_value "system.@system[0].timezone" "UTC" "timezone"
check_value "wireless.default_radio0.ssid" "gchq-2.4" "ssid"
check_value "wireless.default_radio0.key" "test-wifi-plain-password" "wifi key"
check_value "wireless.default_radio0.encryption" "sae-mixed" "encryption"
check_value "network.lan.proto" "static" "lan proto"
check_value "network.lan.ipaddr" "192.168.1.1" "lan ipaddr"
check_value "network.lan.netmask" "255.255.255.0" "lan netmask"
check_value "dropbear.@dropbear[0].PasswordAuth" "off" "dropbear PasswordAuth"

# Verify opkg feeds were written by the full script
FEEDS_CONTENT=$(podman exec "$CONTAINER_NAME" cat /etc/opkg/customfeeds.conf 2>/dev/null || true)
if echo "$FEEDS_CONTENT" | grep -qF "src/gz custom https://example.com/packages"; then
  pass "opkg: customfeeds.conf has correct feed"
else
  fail "opkg: customfeeds.conf missing or incorrect"
fi

# Verify mock opkg was invoked (list-installed + update + install)
OPKG_LOG=$(podman exec "$CONTAINER_NAME" cat /tmp/opkg.log 2>/dev/null || true)
if echo "$OPKG_LOG" | grep -q "list-installed"; then
  pass "opkg: list-installed was called"
else
  fail "opkg: list-installed was not called"
fi
if echo "$OPKG_LOG" | grep -q "update"; then
  pass "opkg: update was called"
else
  fail "opkg: update was not called"
fi

# ── 8. Verify deployment script logic ──
section "8/9 Verifying deployment script features"
TEST_JSON=$(nix build "$PROJECT_ROOT#test-json" --print-out-paths --no-link 2>/dev/null)

check_json() {
  if jq -e "$1" "$TEST_JSON" >/dev/null 2>&1; then
    pass "$2"
  else
    fail "$2 — jq expression failed: $1"
  fi
}

check_json '.packages | length == 2' "packages: 2 packages defined"
check_json '.packages | index("luci") != null' "packages: 'luci' present"
check_json '.packages | index("tcpdump") != null' "packages: 'tcpdump' present"
check_json '.opkg.feeds | length == 1' "feeds: 1 feed defined"
check_json '.opkg.feeds[0] | test("src/gz custom")' "feeds: feed entry correct"
check_json '.sshKeys | length == 1' "sshKeys: 1 key defined"
check_json '.sshKeys[0] | startswith("ssh-ed25519")' "sshKeys: key type correct"
check_json '.settings.wireless.default_radio0.ssid == "gchq-2.4"' "json: ssid in settings"

# ── 9. Test SSH key deployment via deployment script logic ──
section "9/9 Testing SSH key deployment"
# Replicate the deployment script's SSH key deployment against the container
DEPLOY_KEY="ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleKey test@host"
ssh -F "$SSH_CONFIG_PATH" openwrt-test "mkdir -p /etc/dropbear/ && umask 177 && cat > /etc/dropbear/authorized_keys" <<< "$DEPLOY_KEY"
DEPLOYED_KEY=$(podman exec "$CONTAINER_NAME" cat /etc/dropbear/authorized_keys 2>/dev/null || true)
if [ "$DEPLOYED_KEY" = "$DEPLOY_KEY" ]; then
  pass "SSH key deployed correctly via SSH"
else
  fail "SSH key deployment mismatch: got '$DEPLOYED_KEY'"
fi

# ── Result ──
echo ""
if [ "$FAILURES" -eq 0 ]; then
  echo -e "${GREEN}All integration tests passed${NC}"
else
  echo -e "${RED}$FAILURES integration test(s) FAILED${NC}"
  exit 1
fi
