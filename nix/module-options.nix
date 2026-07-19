{ pkgs, lib, ... }:
{
  options.uci = {
    packageManager = lib.mkOption {
      default = "opkg";
      type = lib.types.enum [
        "opkg"
        "apk"
      ];
      description = "Package manager backend: opkg (OpenWrt ≤23.05) or apk (OpenWrt 24.10+).";
    };
    settings = lib.mkOption {
      default = { };
      inherit (pkgs.formats.json { }) type;
    };
    secrets.sops.files = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.path;
      description = "List of sops files to parse and load. All keys in the provided files are merged into one attrset. Key collisions are ignored.";
    };
    packages = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "List of packages to install";
    };
    packageSources.feeds = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "List of custom package feeds/repositories";
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
    rawUci = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "Verbatim `uci` commands for anything the typed model can't express (rename, reorder, deletes). Must each start with 'uci '.";
    };
    files = lib.mkOption {
      default = [ ];
      type = lib.types.listOf (lib.types.submodule {
        options = {
          path = lib.mkOption {
            type = lib.types.str;
            description = "Absolute destination path on the target device.";
          };
          content = lib.mkOption {
            type = lib.types.str;
            description = "Text content to write. Use `base64` instead for binary content.";
          };
          base64 = lib.mkOption {
            default = null;
            type = lib.types.nullOr lib.types.str;
            description = "Base64-encoded binary content. Takes precedence over `content` when set.";
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
      });
      description = "Arbitrary files to write on the target device.";
    };
  };
}
