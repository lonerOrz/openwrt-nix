{
  rustPlatform,
  lib,
}:
let
  src = builtins.path {
    path = ../.;
    name = "nuci-src";
    filter = name: type:
      let
        base = baseNameOf name;
        isInsideSrc = lib.hasPrefix (toString ../. + "/src") name;
      in
      base == "Cargo.toml" ||
      base == "Cargo.lock" ||
      (type == "directory" && base == "src") ||
      isInsideSrc;
  };
in
rustPlatform.buildRustPackage {
  pname = "nuci";
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
