#!/usr/bin/env bash
# Stage/smoke test for `nuci deploy --verbose`.
# Auto mode spins a throwaway apk container; pass TARGET KEY to use a real box.
# In auto mode an ephemeral ed25519 key is generated and its public half injected
# into the container's authorized_keys, so no external SSH key is required.
# The demo config examples/demo.json: its sshKeys placeholder only matters when
# deploying to a real box without passing a key.
set -euo pipefail

cd "$(dirname "$0")/.."

ENGINE="${CONTAINER_ENGINE:-podman}"
IMAGE="openwrt-test-apk-env"
TARGET="${1:-}"
IDENTITY="${2:-}"
PORT=22
CLEANUP_CONTAINER=""
TMPKEY=""

ensure_image() {
  if ! "$ENGINE" image inspect "$IMAGE" >/dev/null 2>&1; then
    echo ">> Building container image $IMAGE ..."
    "$ENGINE" build -q -t "$IMAGE" -f tests/Containerfile.apk . >/dev/null
  fi
}

maybe_start_container() {
  if [[ -z $TARGET ]]; then
    ensure_image
    echo ">> Starting throwaway $IMAGE container ..."
    CLEANUP_CONTAINER="$("$ENGINE" run --rm -d -P "$IMAGE")"
    TARGET="root@127.0.0.1"
    PORT="$("$ENGINE" port "$CLEANUP_CONTAINER" 22/tcp | cut -d: -f2)"
    # Ephemeral key, injected into the container's dropbear authorized_keys,
    # mirroring the integration harness (tests/common/target.rs::inject_ssh_key).
    TMPKEY="$(mktemp -d)/id_ed25519"
    ssh-keygen -t ed25519 -N "" -f "$TMPKEY" -q
    local pub
    pub="$(cat "$TMPKEY.pub")"
    "$ENGINE" exec "$CLEANUP_CONTAINER" sh -c "
      mkdir -p /etc/dropbear
      cat > /etc/dropbear/authorized_keys <<KEYEOF
$pub
KEYEOF
      chmod 700 /etc/dropbear
      chmod 600 /etc/dropbear/authorized_keys
    "
    IDENTITY="$TMPKEY"
    # Give dropbear a moment to (re)read authorized_keys.
    sleep 3
  fi
}

cleanup() {
  [[ -n $CLEANUP_CONTAINER ]] && "$ENGINE" stop "$CLEANUP_CONTAINER" >/dev/null 2>&1 || true
  [[ -n $TMPKEY ]] && rm -rf "$(dirname "$TMPKEY")" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo ">> Building nuci binary ..."
cargo build --bin nuci >/dev/null 2>&1
BIN="$(cargo metadata --format-version=1 --no-deps 2>/dev/null | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/debug/nuci"

maybe_start_container

echo
echo "==================================================================="
echo ">> DEPLOY WITHOUT --verbose (remote stdout is discarded)"
echo "==================================================================="
"$BIN" deploy examples/demo.json --target "$TARGET" --port "$PORT" --identity "$IDENTITY" --force

echo
echo "==================================================================="
echo ">> DEPLOY WITH --verbose (stage banners + set -x command trace)"
echo "==================================================================="
"$BIN" deploy examples/demo.json --target "$TARGET" --port "$PORT" --identity "$IDENTITY" --force --verbose

echo
echo ">> Done. Under --verbose you should see '[nuci] stage: …' banners and"
echo "   '+' traced commands; without it, the same remote output is silent."
