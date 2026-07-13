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
        export PATH="${
          lib.makeBinPath [
            pkgs.openssh
            pkgs.rsync
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
}
