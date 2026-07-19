{
  uci.packageManager = "apk";
  uci.settings = {
    system.system = [
      {
        _type = "system";
        hostname = "rauter-apk";
        timezone = "UTC";
      }
    ];
    wireless = {
      default_radio0 = {
        _type = "wifi-iface";
        device = "radio0";
        network = "lan";
        mode = "ap";
        ssid = "gchq-2.4";
        encryption = "sae-mixed";
        key = "@wifi_password@";
      };
    };
    network = {
      lan = {
        _type = "interface";
        proto = "static";
        ipaddr = "192.168.1.1";
        netmask = "255.255.255.0";
      };
    };
  };
  uci.packages = [
    "tcpdump"
  ];
  uci.packageSources = {
    feeds = [
      "https://example.com/packages"
    ];
    localPackages = [
      "./packages/libuci20250120.apk"
    ];
  };
  uci.secrets =
    if builtins.pathExists ./secrets.enc.json then { sops.files = [ ./secrets.enc.json ]; } else { };
  uci.sshKeys = [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAvctZwmsE8Bxt0WYnHZAdRKERk0YKwwidsG32rY6cf2 openwrt-test"
  ];
}
