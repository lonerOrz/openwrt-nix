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
      res = lib.evalModules {
        modules = [
          {
            _module.args = {
              inherit pkgs;
            };
          }
          ./module-options.nix
          # Secrets forced empty: firmware builds have no SOPS keys, so
          # isBootstrap = true and plain passwords are used instead of @placeholder@.
          {
            uci.secrets.sops.files = lib.mkForce [ ];
          }
          configuration
        ];
      };
      cfg = res.config.uci;

      # Omitting the secrets field means any @placeholder@ in settings fails to compile — catches misconfig early.
      filesJson = map (
        f:
        {
          path = f.path;
          executable = f.executable;
          content =
            if f.base64 != null then
              {
                base64 = f.base64;
              }
            else if f.content != null then
              f.content
            else
              "";
        }
        // (lib.optionalAttrs (f.checksum != null) {
          inherit (f) checksum;
        })
      ) cfg.files;

      uciJson = (formats.json { }).generate "uci.json" {
        inherit (cfg)
          packageManager
          settings
          packages
          packageSources
          sshKeys
          rawUci
          ;
        files = filesJson;
      };

      # single derivation, no IFD
      extraFiles =
        pkgs.runCommand "openwrt-extra-files"
          {
            nativeBuildInputs = [ nuci ];
          }
          ''
            mkdir -p $out/etc/uci-defaults

            # nuci compile output is itself a shell script (touch/while/heredocs).
            # It must NOT be wrapped in `uci batch` — non-uci lines are silently dropped.
            cat > $out/etc/uci-defaults/99-nuci-bootstrap <<'SCRIPT'
            #!/bin/sh
            set -e
            SCRIPT

            ${nuci}/bin/nuci compile --no-sops "${uciJson}" >> $out/etc/uci-defaults/99-nuci-bootstrap

            cat >> $out/etc/uci-defaults/99-nuci-bootstrap <<'SCRIPT'
            uci commit
            SCRIPT

            chmod +x $out/etc/uci-defaults/99-nuci-bootstrap
          '';

      # resolve release
      resolvedRelease =
        if release != null then
          release
        else
          lib.trim (builtins.readFile "${openwrt-imagebuilder}/cache/LATEST_RELEASE");

      # identify hardware profile → target/variant
      profiles = openwrt-imagebuilder.lib.profiles { inherit pkgs; };
      profileConfig = profiles.identifyProfile profile;

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
