{
  description = "OpenWrt router management with Nix";

  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    openwrt-imagebuilder = {
      url = "github:astro/nix-openwrt-imagebuilder";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{
      self,
      flake-parts,
      openwrt-imagebuilder,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [ inputs.treefmt-nix.flakeModule ];

      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      perSystem =
        {
          config,
          pkgs,
          ...
        }:
        let
          uci = pkgs.callPackage ./nix { inherit openwrt-imagebuilder; };
          uciConfig = uci.writeUci ./example.nix;
          testConfig = uci.writeUci ./tests/test_config.nix;
          testConfigApk = uci.writeUci ./tests/test_config_apk.nix;
          isX86Linux = pkgs.stdenv.hostPlatform.isLinux && pkgs.stdenv.hostPlatform.isx86_64;
          exampleFirmware =
            if isX86Linux then
              uci.buildFirmware {
                configuration = ./example.nix;
                profile = "linksys_e8450-ubi";
              }
            else
              null;
        in
        {
          treefmt = {
            projectRootFile = "flake.lock";
            programs = {
              rustfmt.enable = true;
              nixfmt.enable = true;
              shfmt.enable = true;
              yamlfmt.enable = true;
              prettier.enable = true;
            };
            settings.formatter.prettier.includes = [
              "*.md"
              "*.json"
            ];
            settings.global.excludes = [
              "secrets.yml"
              "tests/secrets.enc.json"
            ];
          };

          packages = {
            nuci = uci.nuci;
            default = uci.nuci;
            example-json = uciConfig.json;
            test-json = testConfig.json;
            test-json-apk = testConfigApk.json;
          }
          // (pkgs.lib.optionalAttrs isX86Linux {
            firmware = exampleFirmware;
          });

          apps = {
            example = {
              type = "app";
              program = toString uciConfig.command;
            };
            default = {
              type = "app";
              program = toString uciConfig.command;
            };
          };

          devShells.default = pkgs.mkShell {
            buildInputs = with pkgs; [
              just
              sops
              openssh
              mdbook
              cargo
              rustc
              clippy
              rustfmt
              config.treefmt.build.wrapper
            ];
          };
        };
    };
}
