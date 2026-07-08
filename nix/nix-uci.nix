{
  rustPlatform,
  lib,
}:
rustPlatform.buildRustPackage {
  pname = "nix-uci";
  version = "0.0.1";
  src = ../.;

  cargoLock = {
    lockFile = ../Cargo.lock;
  };

  meta = {
    description = "Write openwrt's UCI configuration using nixos modules";
    homepage = "https://github.com/lonerOrz/openwrt-nix";
    license = lib.licenses.mit;
    maintainers = with lib.maintainers; [ lonerOrz ];
    platforms = lib.platforms.unix;
  };
}
