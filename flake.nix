{
  description = "A free and open source 3D creation suite (upstream binaries)";
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
      {
        packages = {
          inherit (uci) nix-uci;
        };
        # `nix run .#example` will output uci configuration
        apps.example = {
          type = "app";
          program = toString (self.packages.${system}.writeUci ./example.nix).command;
        };
        defaultPackage = self.packages.${system}.nix-uci;
        devShell = pkgs.mkShell {
          buildInputs = [
            pkgs.just
            pkgs.sops
          ];
        };
      }
    );
}
