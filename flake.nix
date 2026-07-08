{
  description = "OpenWrt router management with Nix";
  inputs.nixpkgs.url = "nixpkgs/nixos-unstable";

  outputs =
    {
      self,
      nixpkgs,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      lib = nixpkgs.lib;
    in
    lib.genAttrs systems (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        uci = pkgs.callPackage ./nix { };
      in
      rec {
        packages.nix-uci = uci.nix-uci;
        packages.writeUci = uci.writeUci;
        # `nix run .#example` will output uci configuration
        apps.example = {
          type = "app";
          program = toString (uci.writeUci ./example.nix).command;
        };
        defaultPackage = packages.nix-uci;
        devShell = pkgs.mkShell {
          buildInputs = [
            pkgs.just
            pkgs.sops
          ];
        };
      }
    );
}
