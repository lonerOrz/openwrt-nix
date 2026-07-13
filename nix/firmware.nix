{
  lib,
  pkgs,
  openwrt-imagebuilder,
  formats,
  nuci,
}:

{
  # Build a bootstrap OpenWrt firmware — no secrets, no SOPS.
  # Secrets are pushed later via `nuci deploy` (Day-2).
  #
  # Args:
  #   configuration - path to Nix module (e.g. ./example.nix)
  #   profile       - OpenWrt hardware profile name (e.g. "linksys_e8450-ubi")
  #   release       - OpenWrt release version (default: latest cached)
  buildFirmware =
    {
      configuration,
      profile,
      release ? null,
    }:
    let
      # 1. Evaluate Nix module — override secrets to empty so isBootstrap = true
      #    (plain passwords instead of @placeholder@)
      res = lib.evalModules {
        modules = [
          {
            _module.args = {
              inherit pkgs;
            };
          }
          ./module-options.nix
          # ponytail: force secrets empty — firmware has no SOPS keys in sandbox
          {
            uci.secrets.sops.files = lib.mkForce [ ];
          }
          configuration
        ];
      };
      cfg = res.config.uci;

      # 2. Generate JSON — secrets field omitted so nuci sees no SOPS metadata
      #    Any @placeholder@ in settings will cause compile to error (good — catches misconfig early)
      uciJson = (formats.json { }).generate "uci.json" {
        inherit (cfg)
          packageManager
          settings
          packages
          packageSources
          sshKeys
          ;
      };

      # 3. uci-defaults bootstrap script — single derivation, no IFD
      extraFiles =
        pkgs.runCommand "openwrt-extra-files"
          {
            nativeBuildInputs = [ nuci ];
          }
          ''
            mkdir -p $out/etc/uci-defaults

            ${nuci}/bin/nuci compile --no-sops "${uciJson}" > /tmp/uci_commands

            cat > $out/etc/uci-defaults/99-nuci-bootstrap <<'SCRIPT'
            #!/bin/sh
            uci -q batch <<'UCI'
            SCRIPT

            cat /tmp/uci_commands >> $out/etc/uci-defaults/99-nuci-bootstrap

            cat >> $out/etc/uci-defaults/99-nuci-bootstrap <<'SCRIPT'
            UCI
            uci commit
            rm -f /etc/uci-defaults/99-nuci-bootstrap
            SCRIPT

            chmod +x $out/etc/uci-defaults/99-nuci-bootstrap
          '';

      # 5. Resolve release
      resolvedRelease =
        if release != null then
          release
        else
          lib.trim (builtins.readFile "${openwrt-imagebuilder}/cache/LATEST_RELEASE");

      # 6. Identify hardware profile → target/variant
      profiles = openwrt-imagebuilder.lib.profiles { inherit pkgs; };
      profileConfig = profiles.identifyProfile profile;

      # 7. Build firmware
      build = openwrt-imagebuilder.lib.build;
    in
    build (
      profileConfig
      // {
        inherit (cfg) packages;
        files = extraFiles;
      }
      // lib.optionalAttrs (resolvedRelease != null) {
        release = resolvedRelease;
      }
    );
}
