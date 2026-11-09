{
  pkgs,
  lib,
  config,
  ...
}:
{
  options.uci = {
    packageManager = lib.mkOption {
      default = "opkg";
      type = lib.types.enum [
        "opkg"
        "apk"
      ];
      apply =
        v:
        if v == "opkg" && config.uci.packageSources.apkRepositories != [ ] then
          throw "uci.packageSources.apkRepositories may not be set on an opkg target; use uci.packageSources.opkgFeeds"
        else if v == "apk" && config.uci.packageSources.opkgFeeds != [ ] then
          throw "uci.packageSources.opkgFeeds may not be set on an apk target; use uci.packageSources.apkRepositories"
        else
          v;
      description = "Backend must match the package-source format; a mismatched feed is a throw.";
    };
    settings = lib.mkOption {
      default = { };
      inherit (pkgs.formats.json { }) type;
    };
    secrets.sops.files = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.path;
      description = "Cross-file key collisions (same key, different value) are a hard error.";
    };
    packages = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "A leading '-' removes the package instead of installing it.";
    };
    packageSources.opkgFeeds = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "Written only on opkg targets; mutually exclusive with apkRepositories.";
    };
    packageSources.apkRepositories = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "Written only on apk targets; mutually exclusive with opkgFeeds.";
    };
    packageSources.localPackages = lib.mkOption {
      default = [ ];
      type = lib.types.listOf (lib.types.either lib.types.str lib.types.path);
      description = "Local .ipk/.apk paths, streamed to /tmp on the target.";
    };
    sshKeys = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "Deployed to /etc/dropbear/authorized_keys (mode 0600).";
    };
    watchdogTimeout = lib.mkOption {
      default = 60;
      type = lib.types.int;
      description = "Rollback fires if the handshake is unconfirmed within this window.";
    };
    verbose = lib.mkOption {
      default = false;
      type = lib.types.bool;
      description = "Injects --verbose into deploy; a CLI -v takes precedence.";
    };
    port = lib.mkOption {
      default = null;
      type = lib.types.nullOr lib.types.int;
      description = "Injected into deploy; transport-only, ignored by compile/diff. A CLI --port takes precedence.";
    };
    identity = lib.mkOption {
      default = null;
      type = lib.types.nullOr lib.types.str;
      description = "Injected as --identity, shell-quoted; a string (not path) so the key avoids the Nix store. A CLI -i takes precedence.";
    };
    rawUci = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "Each line must start with 'uci ' (injection guard).";
    };
    files = lib.mkOption {
      default = [ ];
      type = lib.types.listOf (
        lib.types.submodule {
          options = {
            path = lib.mkOption {
              type = lib.types.str;
              description = "Absolute destination on the target.";
            };
            content = lib.mkOption {
              default = null;
              type = lib.types.nullOr lib.types.str;
              description = "Text body; use base64 for binary content.";
            };
            base64 = lib.mkOption {
              default = null;
              type = lib.types.nullOr lib.types.str;
              description = "Binary body; mutually exclusive with content.";
            };
            checksum = lib.mkOption {
              default = null;
              type = lib.types.nullOr lib.types.str;
              description = "Skip the write when the target hash already matches.";
            };
            executable = lib.mkOption {
              default = false;
              type = lib.types.bool;
              description = "Sets 0755 instead of 0644.";
            };
          };
        }
      );
      apply =
        files:
        lib.throwIfNot (lib.all (
          f: f.content == null || f.base64 == null
        ) files) "uci.files entries cannot set both 'content' and 'base64'" files;
      description = "content and base64 are mutually exclusive.";
    };
  };
}
