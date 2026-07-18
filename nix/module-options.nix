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
  };
}
