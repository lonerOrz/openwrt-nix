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
    "-tcpdump"
    "htop"
  ];
  uci.packageSources = {
    feeds = [
      "src/gz openwrt_base https://downloads.openwrt.org/releases/23.05.5/packages/x86_64/base"
      "src/gz openwrt_packages https://downloads.openwrt.org/releases/23.05.5/packages/x86_64/packages"
    ];
  };
  uci.files = [
    {
      path = "/etc/nuci-managed.txt";
      content = "nuci-managed-file-ok\n";
      executable = true;
    }
  ];
  uci.secrets =
    if builtins.pathExists ./secrets.enc.json then { sops.files = [ ./secrets.enc.json ]; } else { };
  uci.rawUci = [
    "uci set nuci_test.marker=escaped"
    "uci commit nuci_test"
  ];
  uci.sshKeys = [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEGPJpRJiBIHwzjGVJxKYGO8nCrhAbHnqHox3X+qkRM8 openwrt-test"
  ];
}
