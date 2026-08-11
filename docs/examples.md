# Configuration Examples

## Basic Network & Firewall

```nix
{
  uci.settings = {
    system.system = [ { _type = "system"; hostname = "rauter"; } ];
    network = {
      lan = { _type = "interface"; device = "br-lan"; proto = "dhcp"; };
      wan = { _type = "interface"; proto = "pppoe"; username = "@wan_user@"; password = "@wan_pass@"; };
    };
    firewall.guest = { _type = "zone"; name = "guest"; network = [ "guest" ]; input = "REJECT"; output = "ACCEPT"; };
  };
  uci.watchdogTimeout = 120;
}
```

## Wireless & SOPS Secrets

```nix
{
  uci.settings.wireless = {
    radio0 = { _type = "wifi-device"; type = "mac80211"; channel = "auto"; band = "2g"; };
    default_radio0 = {
      _type = "wifi-iface"; device = "radio0"; network = "lan";
      mode = "ap"; ssid = "home-2.4"; encryption = "sae-mixed"; key = "@wifi_password@";
    };
  };
  uci.secrets.sops.files = [ ./secrets.yml ];
}
```

## Package Management & Feeds

```nix
{
  uci.packageManager = "opkg"; # or "apk"
  uci.packages = [ "-tcpdump" "htop" ];
  uci.packageSources = {
    feeds = [ "src/gz custom https://dl.openwrt.org/packages" ];
    localPackages = [ "./packages/luci-app-custom_1.0_all.ipk" ];
  };
}
```

## Custom Files

```nix
{
  uci.files = [
    # Text File (POSIX cat heredoc)
    { path = "/etc/config.txt"; content = "key=value\n"; }
    # Binary File (Base64 + SHA256 checksum guard)
    {
      path = "/usr/bin/blob";
      base64 = "aGVsbG8=";
      executable = true;
      checksum = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    }
  ];
}
```

## Escape Hatch (rawUci)

```nix
{
  uci.rawUci = [
    "uci rename network.lan=lan0"
    "uci reorder wireless.@wifi-iface[0]=1"
  ];
}
```

## Day-1 Firmware Build

```nix
exampleFirmware = uci.buildFirmware {
  configuration = ./example.nix;
  profile = "linksys_e8450-ubi";
};
```
