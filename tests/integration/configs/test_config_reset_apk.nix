# UCI-only variant of test_config_apk.nix for offline-safe state resets.
#
# `reset_uci_state` must manage UCI/files/keys/secrets without ever touching the
# package manager, so this imports the single source of truth (test_config_apk.nix)
# and drops only the package-manager keys. No JSON post-processing: the reset
# config is built by nix like every other test config.
(import ./test_config_apk.nix)
// {
  uci = removeAttrs (import ./test_config_apk.nix).uci [
    "packages"
    "packageSources"
  ];
}
