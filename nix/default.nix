{
  formats,
  lib,
  writeShellScript,
  pkgs,
  sops,
  openwrt-imagebuilder ? null,
}:
let
  nuci = pkgs.callPackage ./nuci.nix { };
  firmware =
    if openwrt-imagebuilder != null then
      pkgs.callPackage ./firmware.nix {
        inherit openwrt-imagebuilder nuci;
      }
    else
      null;
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
            else
              f.content;
        }
        // (lib.optionalAttrs (f.checksum != null) {
          inherit (f) checksum;
        })
      ) res.config.uci.files;
      json = (formats.json { }).generate "uci.json" {
        inherit (res.config.uci)
          packageManager
          settings
          secrets
          packages
          packageSources
          sshKeys
          rawUci
          ;
        files = filesJson;
      };
    in
    {
      inherit json;
      command = writeShellScript "uci-commands" ''
        set -euo pipefail
        export PATH="${
          lib.makeBinPath [
            pkgs.openssh
            sops
          ]
        }:$PATH"
        if [ "$#" -lt 1 ]; then
          ${nuci}/bin/nuci compile "${json}"
        else
          ${nuci}/bin/nuci deploy "${json}" --target "$1" --watchdog-timeout "${toString res.config.uci.watchdogTimeout}"
        fi
      '';
    };
  inherit nuci;
  buildFirmware =
    if firmware != null then
      firmware.buildFirmware
    else
      throw "buildFirmware requires 'openwrt-imagebuilder' to be passed to nix/default.nix";
}
