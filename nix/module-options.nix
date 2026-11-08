{ pkgs, lib, config, ... }:
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
      description = "Package manager backend. Use \"apk\" for OpenWrt 25.12+ (default on current stable, apk became the default at 24.10); use \"opkg\" for older releases still on opkg. Runtime discovery verifies the target actually provides the selected backend. The package source list must match the backend: opkg targets use packageSources.opkgFeeds, apk targets use packageSources.apkRepositories; setting the other one is an error.";
    };
    settings = lib.mkOption {
      default = { };
      inherit (pkgs.formats.json { }) type;
    };
    secrets.sops.files = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.path;
      description = "List of sops files to parse and load. All keys in the provided files are merged into one attrset. Key collisions (same key, different value) across files or between the secrets directory and SOPS are a hard error; values must be consistent across sources.";
    };
    packages = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "List of packages to install";
    };
    packageSources.opkgFeeds = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "Opkg-style feeds (src/gz <name> <url>). Written to /etc/opkg/customfeeds.conf only when the target uses opkg. Mutually exclusive with apkRepositories on opkg targets.";
    };
    packageSources.apkRepositories = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "Apk repository URLs. Written to /etc/apk/repositories.d/customfeeds.list only when the target uses apk. Mutually exclusive with opkgFeeds on apk targets.";
    };
    packageSources.localPackages = lib.mkOption {
      default = [ ];
      type = lib.types.listOf (lib.types.either lib.types.str lib.types.path);
      description = "List of local .ipk/.apk file paths to install";
    };
    sshKeys = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "List of SSH authorized keys to deploy to the router";
    };
    watchdogTimeout = lib.mkOption {
      default = 60;
      type = lib.types.int;
      description = "Watchdog timeout in seconds before rollback triggers on deployment.";
    };
    rawUci = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "Verbatim `uci` commands for anything the typed model can't express (rename, reorder, deletes). Must each start with 'uci '.";
    };
    files = lib.mkOption {
      default = [ ];
      type = lib.types.listOf (
        lib.types.submodule {
          options = {
            path = lib.mkOption {
              type = lib.types.str;
              description = "Absolute destination path on the target device.";
            };
            content = lib.mkOption {
              default = null;
              type = lib.types.nullOr lib.types.str;
              description = "Text content to write. Use `base64` instead for binary content.";
            };
            base64 = lib.mkOption {
              default = null;
              type = lib.types.nullOr lib.types.str;
              description = "Base64-encoded binary content. Mutually exclusive with `content`.";
            };
            checksum = lib.mkOption {
              default = null;
              type = lib.types.nullOr lib.types.str;
              description = "Expected sha256 (hex) of the file. When set, the target skips the write if its current hash already matches.";
            };
            executable = lib.mkOption {
              default = false;
              type = lib.types.bool;
              description = "Whether to make the file executable (chmod 755). Default: 644.";
            };
          };
        }
      );
      apply =
        files:
        lib.throwIfNot (lib.all (
          f: f.content == null || f.base64 == null
        ) files) "uci.files entries cannot set both 'content' and 'base64'" files;
      description = "Arbitrary files to write on the target device (content and base64 are mutually exclusive).";
    };
  };
}
