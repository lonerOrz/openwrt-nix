{
  description = "OpenWrt router management with Nix";
  inputs.nixpkgs.url = "nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      lib = nixpkgs.lib;
      forEachSystem = f: lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${system});
    in
    {
      packages = forEachSystem (system: pkgs:
        let
          uci = pkgs.callPackage ./nix { };
          config = uci.writeUci ./example.nix;
        in
        {
          nuci = uci.nuci;
          default = uci.nuci;
          example-json = config.json;
        }
      );

      apps = forEachSystem (system: pkgs:
        let
          uci = pkgs.callPackage ./nix { };
        in
        {
          example = {
            type = "app";
            program = toString (uci.writeUci ./example.nix).command;
          };
          default = self.apps.${system}.example;
        }
      );

      devShells = forEachSystem (system: pkgs: {
        default = pkgs.mkShell {
          buildInputs = [
            pkgs.just
            pkgs.sops
          ];
        };
      });
    };
}
