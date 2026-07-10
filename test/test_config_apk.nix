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
  uci.packages = [ "luci" "tcpdump" ];
  uci.opkg = {
    feeds = [
      "https://example.com/packages"
    ];
    localPackages = [
      "./packages/test-package_1.0_all.apk"
    ];
  };
  uci.secrets.sops.files = [
    ./secrets.enc.json
  ];
  uci.sshKeys = [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleKey test@host"
  ];
}
