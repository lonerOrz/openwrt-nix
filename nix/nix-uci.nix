{
  buildPythonApplication,
  lib,
  runCommand,
}:
buildPythonApplication {
  pname = "nix-uci";
  version = "0.0.1";
  src = runCommand "src" { } ''
    mkdir $out
    cp -r ${../nix_uci} $out/nix_uci
    install ${../setup.cfg} $out/setup.cfg
    install ${../setup.py} $out/setup.py
  '';

  meta = {
    description = "Write openwrt's UCI configuration using nixos modules";
    homepage = "https://github.com/lonerOrz/openwrt-nix";
    license = lib.licenses.mit;
    maintainers = with lib.maintainers; [ lonerOrz ];
    platforms = lib.platforms.unix;
  };
}
