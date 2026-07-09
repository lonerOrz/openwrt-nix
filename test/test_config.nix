{
  uci.settings = {
    system.system = [
      {
        _type = "system";
        hostname = "rauter";
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
        key = "test-wifi-plain-password";
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
      "src/gz custom https://example.com/packages"
    ];
    localPackages = [
      "./packages/test-package_1.0_all.ipk"
    ];
  };
  uci.sshKeys = [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleKey test@host"
  ];
}
