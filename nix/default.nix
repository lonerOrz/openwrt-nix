{
  formats,
  lib,
  writeShellScript,
  pkgs,
}:
let
  nix-uci = pkgs.callPackage ./nix-uci.nix {
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
      json = (formats.json { }).generate "uci.json" { inherit (res.config.uci) settings secrets; };
    in
    {
      inherit json;
      command = writeShellScript "uci-commands" ''
        ${nix-uci}/bin/nix-uci "${json}"
      '';
    };
  inherit nix-uci;
}
