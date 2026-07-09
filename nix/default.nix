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
        inherit (res.config.uci) settings secrets packages opkg sshKeys;
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

        # Deploy SSH authorized keys
        SSH_KEYS=$($JQ -r '.sshKeys[]? // empty' "${json}")
        if [ -n "$SSH_KEYS" ]; then
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
            echo "root:$root_pwd" | $SSH $SSH_OPTS "$TARGET" "chpasswd" >/dev/null 2>&1 || true
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
        $SSH $SSH_OPTS "$TARGET" "( sleep 60; cp -a /tmp/.uci-rollback-backup/* /etc/config/; /etc/init.d/network restart; rm -rf /tmp/.uci-rollback-backup /tmp/.uci-watchdog-pid ) & echo \$! > /tmp/.uci-watchdog-pid"

        # Restart network to apply changes
        $SSH $SSH_OPTS "$TARGET" "/etc/init.d/network restart" || true

        # Wait for target to come back, then kill watchdog
        echo "Waiting for target to come back (60s rollback window)..." >&2
        for i in $(seq 1 30); do
          sleep 2
          if $SSH $SSH_OPTS "$TARGET" "kill \$(cat /tmp/.uci-watchdog-pid) 2>/dev/null"; then
            echo "Connectivity verified, rollback watchdog cancelled." >&2
            break
          fi
        done
        # Cleanup
        $SSH $SSH_OPTS "$TARGET" "rm -rf /tmp/.uci-rollback-backup /tmp/.uci-watchdog-pid" 2>/dev/null || true

        # Setup tinc keys if needed
        $SSH $SSH_OPTS "$TARGET" "if [ ! -f /etc/tinc/retiolum/rsa_key.priv ]; then mkdir -p /etc/tinc/retiolum; tinc -n retiolum generate-keys; /etc/init.d/tinc start; fi"
        $RSYNC -e "ssh $SSH_OPTS" -ac /etc/tinc/retiolum/hosts "$TARGET:/etc/tinc/retiolum"
      '';
    };
  inherit nuci;
}
