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
        inherit (res.config.uci) settings secrets packages;
      };
      sopsFiles = res.config.uci.secrets.sops.files;
    in
    {
      inherit json;
      command = writeShellScript "uci-commands" ''
        set -euo pipefail

        TMP_SECRETS=$(mktemp -d)
        trap 'rm -rf "$TMP_SECRETS"' EXIT

        ${lib.concatMapStringsSep "\n" (file: ''
          if [ -f "${file}" ]; then
            ${sops}/bin/sops -d --output-type json "${file}" > "$TMP_SECRETS/${builtins.hashString "sha256" (toString file)}.json"
          fi
        '') sopsFiles}

        ${nuci}/bin/nuci "${json}" "$TMP_SECRETS"
      '';
    };
  inherit nuci;
}
