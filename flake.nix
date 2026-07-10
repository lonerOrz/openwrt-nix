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
      packages = forEachSystem (
        system: pkgs:
        let
          uci = pkgs.callPackage ./nix { };
          config = uci.writeUci ./example.nix;
          testConfig = uci.writeUci ./test/test_config.nix;
          testConfigApk = uci.writeUci ./test/test_config_apk.nix;
        in
        {
          nuci = uci.nuci;
          default = uci.nuci;
          example-json = config.json;
          test-json = testConfig.json;
          test-json-apk = testConfigApk.json;
        }
      );

      apps = forEachSystem (
        system: pkgs:
        let
          uci = pkgs.callPackage ./nix { };
        in
        {
          example = {
            type = "app";
            program = toString (uci.writeUci ./example.nix).command;
          };
          test-deploy = {
            type = "app";
            program = toString (uci.writeUci ./test/test_config.nix).command;
          };
          test-deploy-apk = {
            type = "app";
            program = toString (uci.writeUci ./test/test_config_apk.nix).command;
          };
          default = self.apps.${system}.example;
        }
      );

      devShells = forEachSystem (
        system: pkgs: {
          default = pkgs.mkShell {
            buildInputs = with pkgs; [
              just
              sops
              cargo
              rustc
            ];
          };
        }
      );
    };
}
