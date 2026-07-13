{
  formats,
  lib,
  writeShellScript,
  pkgs,
  sops,
  openwrt-imagebuilder,
}:
let
  nuci = pkgs.callPackage ./nuci.nix { };
  firmware = pkgs.callPackage ./firmware.nix {
    inherit openwrt-imagebuilder nuci;
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
          packageSources
          sshKeys
          ;
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
          ${nuci}/bin/nuci deploy "${json}" --target "$1"
        fi
      '';
    };
  inherit nuci;
  inherit (firmware) buildFirmware;
}
