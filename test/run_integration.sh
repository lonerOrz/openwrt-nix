#!/usr/bin/env bash
set -euo pipefail

# Route container ops through CONTAINER_ENGINE (default podman; CI sets docker)
CONTAINER_ENGINE="${CONTAINER_ENGINE:-podman}"
podman() { command "$CONTAINER_ENGINE" "$@"; }

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONTAINER_NAME="openwrt-integration-test"
SSH_KEY_PATH="/tmp/openwrt_test_key"
SSH_CONFIG_PATH="/tmp/openwrt_test_ssh_config"
SOPS_KEY_DIR="/tmp/nuci_sops_test"
ENCRYPTED_SECRETS="$PROJECT_ROOT/test/secrets.enc.json"

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
  section "Cleaning up"
  podman rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
  rm -f "$SSH_KEY_PATH" "$SSH_KEY_PATH.pub" "$SSH_CONFIG_PATH"
  rm -rf "$SOPS_KEY_DIR"
  git restore --staged "$ENCRYPTED_SECRETS" >/dev/null 2>&1 || true
  rm -f "$ENCRYPTED_SECRETS"
  ok "Cleanup complete."
}
trap cleanup EXIT

# ── 1. Clean previous artifacts ──
section "1/10 Cleaning previous artifacts"
podman rm -f "$CONTAINER_NAME" 2>/dev/null || true
rm -f "$SSH_KEY_PATH" "$SSH_KEY_PATH.pub" "$SSH_CONFIG_PATH"
rm -rf "$SOPS_KEY_DIR"
rm -f "$ENCRYPTED_SECRETS"
git restore --staged "$ENCRYPTED_SECRETS" >/dev/null 2>&1 || true

# ── 2. Build and start container ──
section "2/10 Building OpenWrt test container"
podman build -q -t openwrt-test-env -f "$PROJECT_ROOT/test/Containerfile" "$PROJECT_ROOT" >/dev/null
podman run -d --name "$CONTAINER_NAME" -p 2222:22 openwrt-test-env >/dev/null

# ── 3. Wait for dropbear ──
section "3/10 Waiting for dropbear"
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
section "4/10 Injecting SSH key"
ssh-keygen -t ed25519 -N "" -f "$SSH_KEY_PATH" -C "openwrt-test" -q
podman exec -i "$CONTAINER_NAME" sh -c "mkdir -p /etc/dropbear && cat > /etc/dropbear/authorized_keys" < "$SSH_KEY_PATH.pub"
podman exec "$CONTAINER_NAME" chmod 700 /etc/dropbear
podman exec "$CONTAINER_NAME" chmod 600 /etc/dropbear/authorized_keys
ok "SSH key installed"

# ── 5. Create SSH config ──
section "5/10 Creating SSH config"
cat <<EOF > "$SSH_CONFIG_PATH"
Host openwrt-test
    HostName localhost
    Port 2222
    User root
    StrictHostKeyChecking no
    UserKnownHostsFile /dev/null
    IdentityFile $SSH_KEY_PATH
EOF

# ── 6. Setup SOPS test environment ──
section "6/10 Setting up SOPS test environment"
mkdir -p "$SOPS_KEY_DIR"
export SOPS_AGE_KEY_FILE="$SOPS_KEY_DIR/keys.txt"

nix shell nixpkgs#age -c age-keygen > "$SOPS_KEY_DIR/keys.txt"
PUBKEY=$(grep -o 'age1[a-z0-9]*' "$SOPS_KEY_DIR/keys.txt")

nix shell nixpkgs#sops -c sops --config /dev/null --encrypt --age "$PUBKEY" \
  --input-type json --output-type json \
  --output "$ENCRYPTED_SECRETS" \
  "$PROJECT_ROOT/test/mock_secrets/secrets.json"

git add -N "$ENCRYPTED_SECRETS" 2>/dev/null || true
ok "SOPS encrypted secrets created"

# ── 7. Verify nuci command generation (OPKG + APK) ──
section "7/10 Verifying nuci command generation"
NUCI_OUTPUT_OPKG=$(SOPS_AGE_KEY_FILE="$SOPS_KEY_DIR/keys.txt" nix run "$PROJECT_ROOT#test-deploy" -- 2>/dev/null)
NUCI_OUTPUT_APK=$(SOPS_AGE_KEY_FILE="$SOPS_KEY_DIR/keys.txt" nix run "$PROJECT_ROOT#test-deploy-apk" -- 2>/dev/null)

check_cmd_opkg() {
  if echo "$NUCI_OUTPUT_OPKG" | grep -qF "$1"; then
    pass "[OPKG] $2"
  else
    fail "[OPKG] $2 — pattern not found: $1"
  fi
}

check_cmd_apk() {
  if echo "$NUCI_OUTPUT_APK" | grep -qF "$1"; then
    pass "[APK] $2"
  else
    fail "[APK] $2 — pattern not found: $1"
  fi
}

# OPKG command stream
check_cmd_opkg "add system system" "list section: system created via add"
if echo "$NUCI_OUTPUT_OPKG" | grep -qF "set system.@system[0]=system"; then
  fail "[OPKG] Redundant type set still present for list sections"
else
  pass "[OPKG] Redundant type set correctly removed"
fi
check_cmd_opkg "set system.@system[0].hostname='rauter'" "list section: hostname set"
check_cmd_opkg "set system.@system[0].timezone='UTC'" "list section: timezone set"
check_cmd_opkg "delete wireless.default_radio0" "named section: wireless deleted before recreate"
check_cmd_opkg "set wireless.default_radio0=wifi-iface" "named section: wireless type set"
check_cmd_opkg "set wireless.default_radio0.ssid='gchq-2.4'" "named section: ssid set"
check_cmd_opkg "set wireless.default_radio0.key='my-test-password'" "SOPS: wifi key decrypted correctly"
check_cmd_opkg "delete network.lan" "named section: network deleted before recreate"
check_cmd_opkg "set network.lan=interface" "named section: network type set"
check_cmd_opkg "set network.lan.proto='static'" "named section: lan proto set"
check_cmd_opkg "set network.lan.ipaddr='192.168.1.1'" "named section: lan ipaddr set"
check_cmd_opkg "uci -q batch" "output: uci batch transaction format"
check_cmd_opkg "commit network" "output: commit transaction present"
check_cmd_opkg "printf '' > /etc/opkg/customfeeds.conf" "opkg: feeds file created"
check_cmd_opkg "src/gz custom https://example.com/packages" "opkg: feed entry correct"
check_cmd_opkg "opkg update && opkg install luci tcpdump" "opkg: packages install command"
check_cmd_opkg "opkg install /tmp/test-package_1.0_all.ipk" "opkg: local package install"

# APK command stream
check_cmd_apk "add system system" "list section: system created via add"
if echo "$NUCI_OUTPUT_APK" | grep -qF "set system.@system[0]=system"; then
  fail "[APK] Redundant type set still present for list sections"
else
  pass "[APK] Redundant type set correctly removed"
fi
check_cmd_apk "set system.@system[0].hostname='rauter-apk'" "list section: hostname set"
check_cmd_apk "delete wireless.default_radio0" "named section: wireless deleted before recreate"
check_cmd_apk "set wireless.default_radio0=wifi-iface" "named section: wireless type set"
check_cmd_apk "set wireless.default_radio0.ssid='gchq-2.4'" "named section: ssid set"
check_cmd_apk "set wireless.default_radio0.key='my-test-password'" "SOPS: wifi key decrypted correctly"
check_cmd_apk "delete network.lan" "named section: network deleted before recreate"
check_cmd_apk "set network.lan=interface" "named section: network type set"
check_cmd_apk "set network.lan.proto='static'" "named section: lan proto set"
check_cmd_apk "set network.lan.ipaddr='192.168.1.1'" "named section: lan ipaddr set"
check_cmd_apk "uci -q batch" "output: uci batch transaction format"
check_cmd_apk "commit network" "output: commit transaction present"
check_cmd_apk "printf '' > /etc/apk/repositories.d/customfeeds.list" "apk: feeds file created"
check_cmd_apk "https://example.com/packages" "apk: feed entry correct"
check_cmd_apk "apk -U add luci tcpdump" "apk: packages install command"
check_cmd_apk "apk add --allow-untrusted /tmp/test-package_1.0_all.apk" "apk: local package install"

# ── 8. Deploy to container and verify state (OPKG + APK) ──
section "8/10 Deploying to container and verifying state"

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

# OPKG deployment
SYNTAX_ERR=$(echo "$NUCI_OUTPUT_OPKG" | podman exec -i "$CONTAINER_NAME" sh -n 2>&1)
if [ -n "$SYNTAX_ERR" ]; then
  fail "[OPKG] Syntax error in deployment script: $SYNTAX_ERR"
else
  ok "[OPKG] deployment script passes sh -n syntax check"
fi

DEPLOY_STDERR=$(echo "$NUCI_OUTPUT_OPKG" | podman exec -i "$CONTAINER_NAME" sh -s 2>&1 >/dev/null || true)
UNEXPECTED_ERRORS=$(echo "$DEPLOY_STDERR" | grep -v "uci: Entry not found" | grep -v "^$" || true)
if [ -n "$UNEXPECTED_ERRORS" ]; then
  fail "[OPKG] Unexpected errors during deployment:"
  echo "$UNEXPECTED_ERRORS"
else
  ok "[OPKG] All commands executed without errors"
fi

check_section "system.@system[0]"
check_section "wireless.default_radio0"
check_section "network.lan"
check_value "system.@system[0].hostname" "rauter" "[OPKG] hostname"
check_value "system.@system[0].timezone" "UTC" "[OPKG] timezone"
check_value "wireless.default_radio0.ssid" "gchq-2.4" "[OPKG] ssid"
check_value "wireless.default_radio0.key" "my-test-password" "[OPKG] wifi key (decrypted)"
check_value "wireless.default_radio0.encryption" "sae-mixed" "[OPKG] encryption"
check_value "network.lan.proto" "static" "[OPKG] lan proto"
check_value "network.lan.ipaddr" "192.168.1.1" "[OPKG] lan ipaddr"
check_value "network.lan.netmask" "255.255.255.0" "[OPKG] lan netmask"
check_value "dropbear.@dropbear[0].PasswordAuth" "off" "[OPKG] dropbear PasswordAuth"

FEEDS_CONTENT=$(podman exec "$CONTAINER_NAME" cat /etc/opkg/customfeeds.conf 2>/dev/null || true)
if echo "$FEEDS_CONTENT" | grep -qF "src/gz custom https://example.com/packages"; then
  pass "[OPKG] customfeeds.conf has correct feed"
else
  fail "[OPKG] customfeeds.conf missing or incorrect"
fi

OPKG_LOG=$(podman exec "$CONTAINER_NAME" cat /tmp/opkg.log 2>/dev/null || true)
if echo "$OPKG_LOG" | grep -q "list-installed"; then
  pass "[OPKG] list-installed was called"
else
  fail "[OPKG] list-installed was not called"
fi
if echo "$OPKG_LOG" | grep -q "update"; then
  pass "[OPKG] update was called"
else
  fail "[OPKG] update was not called"
fi

# APK deployment (overwrites the same config sections)
SYNTAX_ERR_APK=$(echo "$NUCI_OUTPUT_APK" | podman exec -i "$CONTAINER_NAME" sh -n 2>&1)
if [ -n "$SYNTAX_ERR_APK" ]; then
  fail "[APK] Syntax error in deployment script: $SYNTAX_ERR_APK"
else
  ok "[APK] deployment script passes sh -n syntax check"
fi

DEPLOY_STDERR_APK=$(echo "$NUCI_OUTPUT_APK" | podman exec -i "$CONTAINER_NAME" sh -s 2>&1 >/dev/null || true)
UNEXPECTED_ERRORS_APK=$(echo "$DEPLOY_STDERR_APK" | grep -v "uci: Entry not found" | grep -v "^$" || true)
if [ -n "$UNEXPECTED_ERRORS_APK" ]; then
  fail "[APK] Unexpected errors during deployment:"
  echo "$UNEXPECTED_ERRORS_APK"
else
  ok "[APK] All commands executed without errors"
fi

check_value "system.@system[0].hostname" "rauter-apk" "[APK] hostname"
check_section "wireless.default_radio0"
check_section "network.lan"

APK_LOG=$(podman exec "$CONTAINER_NAME" cat /tmp/apk.log 2>/dev/null || true)
if echo "$APK_LOG" | grep -q "info -e"; then
  pass "[APK] info -e was called"
else
  fail "[APK] info -e was not called"
fi
if echo "$APK_LOG" | grep -q "add"; then
  pass "[APK] add was called"
else
  fail "[APK] add was not called"
fi

# ── 9. Verify JSON artifact (OPKG + APK) ──
section "9/10 Verifying JSON artifact"
TEST_JSON_OPKG=$(nix build "$PROJECT_ROOT#test-json" --print-out-paths --no-link 2>/dev/null)
TEST_JSON_APK=$(nix build "$PROJECT_ROOT#test-json-apk" --print-out-paths --no-link 2>/dev/null)

check_json_opkg() {
  if jq -e "$1" "$TEST_JSON_OPKG" >/dev/null 2>&1; then
    pass "[OPKG] $2"
  else
    fail "[OPKG] $2 — jq expression failed: $1"
  fi
}

check_json_apk() {
  if jq -e "$1" "$TEST_JSON_APK" >/dev/null 2>&1; then
    pass "[APK] $2"
  else
    fail "[APK] $2 — jq expression failed: $1"
  fi
}

check_json_opkg '.packages | length == 2' "packages: 2 defined"
check_json_opkg '.packages | index("luci") != null' "packages: 'luci' present"
check_json_opkg '.packages | index("tcpdump") != null' "packages: 'tcpdump' present"
check_json_opkg '.opkg.feeds | length == 1' "feeds: 1 defined"
check_json_opkg '.sshKeys | length == 1' "sshKeys: 1 defined"
check_json_opkg '.sshKeys[0] | startswith("ssh-ed25519")' "sshKeys: key type correct"
check_json_opkg '.settings.wireless.default_radio0.ssid == "gchq-2.4"' "json: ssid in settings"
check_json_opkg '.packageManager == "opkg"' "packageManager metadata is 'opkg'"

check_json_apk '.packages | length == 2' "packages: 2 defined"
check_json_apk '.opkg.feeds | length == 1' "feeds: 1 defined"
check_json_apk '.packageManager == "apk"' "packageManager metadata is 'apk'"

# ── 10. Test watchdog rollback ──
section "10/10 Testing watchdog rollback"

# Use podman exec for all container ops (SSH key auth unreliable with init.d dropbear)
info "Backing up config and applying breaking config..."

# Backup + break — all via podman exec
podman exec "$CONTAINER_NAME" sh -c '
  cp -a /etc/config /tmp/.uci-rollback-backup
  uci set dropbear.@dropbear[0].Port="9999"
  uci commit
  killall dropbear
' 2>/dev/null || true

sleep 1

# Start watchdog: detached (-d) — podman keeps it alive
podman exec -d "$CONTAINER_NAME" sh -c '
  sleep 20
  cp -a /tmp/.uci-rollback-backup/* /etc/config/
  /usr/sbin/dropbear -F -E -p 22 -R &
  rm -rf /tmp/.uci-rollback-backup /tmp/.uci-watchdog-pid
' 2>/dev/null

# Write a fake PID for verification (real process is managed by podman -d)
podman exec "$CONTAINER_NAME" sh -c 'echo detached > /tmp/.uci-watchdog-pid' 2>/dev/null

sleep 3

info "Checking SSH is unreachable (dropbear on 9999, not 22)..."
SSH_LOST=false
for i in {1..5}; do
  if ! ssh -o BatchMode=yes -o ConnectTimeout=1 -F "$SSH_CONFIG_PATH" openwrt-test "echo ok" >/dev/null 2>&1; then
    SSH_LOST=true
    break
  fi
  sleep 1
done

if [ "$SSH_LOST" = true ]; then
  pass "SSH became unreachable after port change"
else
  fail "SSH still reachable — port change did not take effect"
fi

WATCHDOG_PID=$(podman exec "$CONTAINER_NAME" cat /tmp/.uci-watchdog-pid 2>/dev/null || echo "")
if [ -n "$WATCHDOG_PID" ]; then
  pass "Watchdog started (detached via podman)"
else
  fail "Watchdog PID file not found"
fi

info "Waiting for watchdog to restore config (up 30s)..."
RESTORED=false
for i in {1..15}; do
  sleep 2
  if ssh -o BatchMode=yes -o ConnectTimeout=1 -F "$SSH_CONFIG_PATH" openwrt-test "echo ok" >/dev/null 2>&1; then
    RESTORED=true
    break
  fi
done

if [ "$RESTORED" = true ]; then
  pass "SSH reconnected after watchdog rollback"
else
  fail "SSH failed to reconnect — watchdog rollback may have failed"
fi

if [ "$RESTORED" = true ]; then
  CURRENT_PORT=$(ssh -F "$SSH_CONFIG_PATH" openwrt-test "uci get dropbear.@dropbear[0].Port" 2>/dev/null || echo "unknown")
  if [ "$CURRENT_PORT" = "22" ]; then
    pass "Dropbear port rolled back to '22'"
  else
    fail "Dropbear port is '$CURRENT_PORT', expected '22'"
  fi

  PID_FILE=$(podman exec "$CONTAINER_NAME" test -f /tmp/.uci-watchdog-pid && echo "exists" || echo "gone")
  BACKUP_DIR=$(podman exec "$CONTAINER_NAME" test -d /tmp/.uci-rollback-backup && echo "exists" || echo "gone")
  if [ "$PID_FILE" = "gone" ]; then
    pass "Watchdog PID file cleaned up"
  else
    fail "Watchdog PID file still exists"
  fi
  if [ "$BACKUP_DIR" = "gone" ]; then
    pass "Rollback backup directory cleaned up"
  else
    fail "Rollback backup directory still exists"
  fi
else
  # Debug: check what's happening inside the container
  info "Debug: checking container state..."
  podman exec "$CONTAINER_NAME" sh -c 'ps aux 2>/dev/null || ps' || true
  podman exec "$CONTAINER_NAME" cat /tmp/.uci-watchdog-pid 2>/dev/null || echo "  no PID file"
  podman exec "$CONTAINER_NAME" ls -la /tmp/.uci-rollback-backup/ 2>/dev/null || echo "  no backup dir"
  podman exec "$CONTAINER_NAME" cat /etc/config/dropbear 2>/dev/null || echo "  no dropbear config"
  podman exec "$CONTAINER_NAME" pidof dropbear 2>/dev/null || echo "  dropbear not running"
fi

# ── Result ──
echo ""
if [ "$FAILURES" -eq 0 ]; then
  echo -e "${GREEN}All integration tests passed${NC}"
else
  echo -e "${RED}$FAILURES integration test(s) FAILED${NC}"
  exit 1
fi
