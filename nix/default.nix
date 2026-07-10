{
  formats,
  lib,
  writeShellScript,
  pkgs,
  sops,
}:
let
  nuci = pkgs.callPackage ./nuci.nix {
    rustPlatform = pkgs.makeRustPlatform {
      cargo = pkgs.cargo;
      rustc = pkgs.rustc;
    };
  };
in
{
  writeUci =
    configuration:
    let
      res = lib.evalModules {
        modules = [
          {
            _module.args = {
              inherit pkgs;
            };
          }
          ./module-options.nix
          configuration
        ];
      };
      json = (formats.json { }).generate "uci.json" {
        inherit (res.config.uci)
          packageManager
          settings
          secrets
          packages
          opkg
          sshKeys
          ;
      };
      sopsFiles = res.config.uci.secrets.sops.files;
    in
    {
      inherit json;
      command = writeShellScript "uci-commands" ''
        set -euo pipefail

        # Use Nix-sealed binaries — no global jq/ssh/scp needed
        JQ="${pkgs.jq}/bin/jq"

        # Decrypt sops secrets
        TMP_SECRETS=$(mktemp -d)
        trap 'rm -rf "$TMP_SECRETS"' EXIT

        ${lib.concatMapStringsSep "\n" (file: ''
          if [ -f "${file}" ]; then
            ${sops}/bin/sops -d --output-type json "${file}" > "$TMP_SECRETS/${builtins.hashString "sha256" (toString file)}.json"
          fi
        '') sopsFiles}

        # No target: output UCI commands to stdout (for dry-run / eval-config)
        if [ "$#" -lt 1 ]; then
          ${nuci}/bin/nuci "${json}" "$TMP_SECRETS"
          exit 0
        fi

        TARGET="$1"
        SSH="${pkgs.openssh}/bin/ssh"
        SCP="${pkgs.openssh}/bin/scp"
        RSYNC="${pkgs.rsync}/bin/rsync"
        SSH_OPTS="''${SSH_OPTS:--o ControlMaster=auto -o ControlPath=/tmp/ssh-%r@%h:%p -o ControlPersist=5m}"

        # Deploy SSH authorized keys (with lockout prevention)
        SSH_KEYS=$($JQ -r '.sshKeys[]? // empty' "${json}")
        DEPLOYER_KEY=$(${pkgs.openssh}/bin/ssh-add -L 2>/dev/null | head -1 || true)
        if [ -n "$SSH_KEYS" ]; then
          # Ensure deployer's current key is in the new key list to prevent lockout
          if [ -n "$DEPLOYER_KEY" ]; then
            PUB_KEY=$(echo "$DEPLOYER_KEY" | cut -d' ' -f1,2)
            if ! echo "$SSH_KEYS" | grep -qF "$PUB_KEY"; then
              echo "⚠ Deployer key not found in new config, appending to prevent lockout..." >&2
              SSH_KEYS=$(printf '%s\n%s' "$SSH_KEYS" "$DEPLOYER_KEY")
            fi
          fi
          echo "Deploying SSH keys to $TARGET..." >&2
          $SSH $SSH_OPTS "$TARGET" "mkdir -p /etc/dropbear/ && umask 177 && cat > /etc/dropbear/authorized_keys" <<KEYS
        $SSH_KEYS
        KEYS
        fi

        # Sync root password from decrypted secrets
        for sec_file in "$TMP_SECRETS"/*.json; do
          [ -f "$sec_file" ] || continue
          root_pwd=$($JQ -r 'select(.root_password != null) | .root_password' "$sec_file" 2>/dev/null || true)
          if [ -n "$root_pwd" ]; then
            echo "Syncing root password..." >&2
            printf 'root:%s\n' "$root_pwd" | $SSH $SSH_OPTS "$TARGET" "chpasswd" >/dev/null 2>&1 || true
            break
          fi
        done

        # Transfer local IPK packages to router /tmp
        LOCAL_PKGS=$($JQ -r '.opkg.localPackages[]? // empty' "${json}")
        for pkg in $LOCAL_PKGS; do
          if [ -f "$pkg" ]; then
            echo "Transferring $(basename "$pkg") to $TARGET:/tmp/ ..." >&2
            $SCP $SSH_OPTS "$pkg" "$TARGET:/tmp/$(basename "$pkg")"
          fi
        done

        # Backup current config before applying changes
        echo "Backing up /etc/config/ on $TARGET..." >&2
        $SSH $SSH_OPTS "$TARGET" "cp -a /etc/config /tmp/.uci-rollback-backup"

        # Generate and apply UCI configuration
        ${nuci}/bin/nuci "${json}" "$TMP_SECRETS" | $SSH $SSH_OPTS "$TARGET" 'sh -s'

        # Start rollback watchdog on target (60s timeout)
        # PID saved on target; deployer kills it after successful reconnection
        $SSH $SSH_OPTS "$TARGET" "( sleep 60; cp -a /tmp/.uci-rollback-backup/* /etc/config/; if [ -x /sbin/reload_config ]; then /sbin/reload_config; else /etc/init.d/network restart; fi || true; rm -rf /tmp/.uci-rollback-backup /tmp/.uci-watchdog-pid ) & echo \$! > /tmp/.uci-watchdog-pid"

        # Restart services to apply changes gracefully
        $SSH $SSH_OPTS "$TARGET" "if [ -x /sbin/reload_config ]; then /sbin/reload_config; else /etc/init.d/network restart; fi" || true

        # Wait for target to come back, then kill watchdog
        echo "Waiting for target to come back (60s rollback window)..." >&2
        CONNECTED=false
        for i in $(seq 1 30); do
          sleep 2
          if $SSH $SSH_OPTS "$TARGET" "kill \$(cat /tmp/.uci-watchdog-pid) 2>/dev/null"; then
            echo "Connectivity verified, rollback watchdog cancelled." >&2
            CONNECTED=true
            break
          fi
        done
        if [ "$CONNECTED" = false ]; then
          echo "Error: Failed to reconnect to $TARGET within 60s. Target may have rolled back." >&2
          exit 1
        fi
        # Cleanup
        $SSH $SSH_OPTS "$TARGET" "rm -rf /tmp/.uci-rollback-backup /tmp/.uci-watchdog-pid" 2>/dev/null || true

        # Setup tinc keys if needed
        $SSH $SSH_OPTS "$TARGET" "if [ ! -f /etc/tinc/retiolum/rsa_key.priv ]; then mkdir -p /etc/tinc/retiolum; tinc -n retiolum generate-keys; /etc/init.d/tinc start; fi"
        $RSYNC -e "ssh $SSH_OPTS" -ac /etc/tinc/retiolum/hosts "$TARGET:/etc/tinc/retiolum"
      '';
    };
  inherit nuci;
}
