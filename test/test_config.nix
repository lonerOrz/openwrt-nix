{
  uci.settings = {
    system.system = [
      {
        _type = "system";
        hostname = "rauter";
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
  };
  uci.packages = [ ];
}
