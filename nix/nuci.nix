{
  rustPlatform,
  lib,
}:
let
  cargoToml = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  src = builtins.path {
    path = ../.;
    name = "nuci-src";
    filter =
      name: type:
      let
        base = baseNameOf name;
        isInsideSrc = lib.hasPrefix (toString ../. + "/src") name;
        # tests/samples/** holds lib-test snapshot fixtures (cargo test --lib);
        # the Nix build runs the lib suite, so they must be in the sandbox.
        isInsideTests = lib.hasPrefix (toString ../. + "/tests") name;
      in
      base == "Cargo.toml"
      || base == "Cargo.lock"
      || (type == "directory" && base == "src")
      || (type == "directory" && base == "tests")
      || isInsideSrc
      || isInsideTests;
  };
in
rustPlatform.buildRustPackage {
  pname = cargoToml.package.name;
  version = cargoToml.package.version;
  inherit src;
  cargoTestFlags = [ "--lib" ];
  cargoLock = {
    lockFile = ./../Cargo.lock;
  };

  meta = {
    description = "Declarative OpenWrt UCI configuration compiler and deployer";
    homepage = "https://github.com/lonerOrz/openwrt-nix";
    license = lib.licenses.mit;
    maintainers = with lib.maintainers; [ lonerOrz ];
    platforms = lib.platforms.unix;
  };
}
