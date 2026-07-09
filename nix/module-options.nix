{ pkgs, lib, ... }:
{
  options.uci = {
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
      description = "List of packages to install via opkg";
    };
    opkg.feeds = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "List of custom opkg feeds to write to /etc/opkg/customfeeds.conf";
    };
    opkg.localPackages = lib.mkOption {
      default = [ ];
      type = lib.types.listOf (lib.types.either lib.types.str lib.types.path);
      description = "List of local .ipk file paths or fetched packages from Nix store to install";
    };
    sshKeys = lib.mkOption {
      default = [ ];
      type = lib.types.listOf lib.types.str;
      description = "List of SSH authorized keys to deploy to the router";
    };
  };
}
