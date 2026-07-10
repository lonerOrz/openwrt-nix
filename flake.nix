{
  description = "OpenWrt router management with Nix";

  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{
      self,
      nixpkgs,
      flake-parts,
      treefmt-nix,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [ inputs.treefmt-nix.flakeModule ];

      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      perSystem =
        {
          config,
          self',
          inputs',
          pkgs,
          system,
          ...
        }:
        let
          uci = pkgs.callPackage ./nix { };
          uciConfig = uci.writeUci ./example.nix;
          testConfig = uci.writeUci ./test/test_config.nix;
          testConfigApk = uci.writeUci ./test/test_config_apk.nix;
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
            # 约束 prettier 的工作范围为 Markdown 和 JSON
            settings.formatter.prettier.includes = [
              "*.md"
              "*.json"
            ];
            # 防御性排除：防止格式化工具因意外美化损坏 SOPS 加密数据和 MAC 校验
            settings.global.excludes = [
              "secrets.yml"
              "test/secrets.enc.json"
            ];
          };

          packages = {
            nuci = uci.nuci;
            default = uci.nuci;
            example-json = uciConfig.json;
            test-json = testConfig.json;
            test-json-apk = testConfigApk.json;
          };

          apps = rec {
            example = {
              type = "app";
              program = toString uciConfig.command;
            };
            test-deploy = {
              type = "app";
              program = toString testConfig.command;
            };
            test-deploy-apk = {
              type = "app";
              program = toString testConfigApk.command;
            };
            default = example;
          };

          devShells.default = pkgs.mkShell {
            buildInputs = with pkgs; [
              just
              sops
              cargo
              rustc
              config.treefmt.build.wrapper # 自动绑定编译好的 treefmt 封装工具
            ];
          };
        };
    };
}
