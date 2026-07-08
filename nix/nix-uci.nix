{
  rustPlatform,
  lib,
}:
let
  src = builtins.path {
    path = ../.;
    name = "nix-uci-src";
    filter =
      name: type:
        let
          base = baseNameOf name;
        in
        base != ".git" && base != ".gitignore";
  };
in
rustPlatform.buildRustPackage {
  pname = "nix-uci";
  version = "0.0.1";
  inherit src;

  cargoLock = {
    lockFile = ./../Cargo.lock;
  };

  meta = {
    description = "Write openwrt's UCI configuration using nixos modules";
    homepage = "https://github.com/lonerOrz/openwrt-nix";
    license = lib.licenses.mit;
    maintainers = with lib.maintainers; [ lonerOrz ];
    platforms = lib.platforms.unix;
  };
}
