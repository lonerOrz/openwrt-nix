# Nix Configuration Examples

Copy-paste-friendly snippets. The Nix module is exposed as `writeUci` from the
flake; the result is a derivation producing `uci.json` plus a `command` script
that runs `nuci compile`/`deploy`.

```nix
# flake outputs (perSystem)
uci = pkgs.callPackage ./nix { inherit openwrt-imagebuilder; };
myRouter = uci.writeUci ./my-router.nix;
# myRouter.json         -> the compiled uci.json
# myRouter.command      -> ./result/bin/uci-commands  (compile or deploy)
```

## Basic network + firewall

```nix
{
  uci.settings = {
    system.system = [ { _type = "system"; hostname = "rauter"; } ];

    network = {
      lan = {
        _type = "interface";
        device = "br-lan";
        proto = "dhcp";
      };
      wan = {
        _type = "interface";
        proto = "pppoe";
        username = "@wan_user@";
        password = "@wan_pass@";
      };
      device = [ {
        _type = "device";
        name = "br-lan";
        type = "bridge";
        ports = [ "lan1" "lan2" "lan3" "lan4" ];
      } ];
    };

    firewall = {
      # A custom zone
      guest = {
        _type = "zone";
        name = "guest";
        network = [ "guest" ];
        input = "REJECT";
        output = "ACCEPT";
        forward = "REJECT";
      };
    };
  };
}
```

## Wireless + SOPS secrets

SOPS placeholders use `@name@`. The encrypted file is decrypted at compile time.

```nix
{
  uci.settings = {
    wireless = {
      radio0 = {
        _type = "wifi-device";
        type = "mac80211";
        channel = "auto";
        band = "2g";
        country = "DE";
      };
      default_radio0 = {
        _type = "wifi-iface";
        device = "radio0";
        network = "lan";
        mode = "ap";
        ssid = "home-2.4";
        encryption = "sae-mixed";
        key = "@wifi_password@";   # resolved from sops
      };
    };
  };

  uci.secrets = {
    sops.files = [ ./secrets.yml ];
  };
}
```

## Packages, custom feeds, local .ipk

```nix
{
  uci.packages = [ "luci" "tcpdump" "htop" ];

  uci.packageSources = {
    feeds = [
      "src/gz kiddin9 https://dl.openwrt.ai/packages/aarch64_cortex-a53/kiddin9"
    ];
    # Real .ipk files, committed or fetched
    localPackages = [
      "./packages/luci-app-nlbwmon_0.3-1_all.ipk"
    ];
  };
}
```

## Custom init.d service (nftables)

See [Design Philosophy](features.md) for why this is the clean pattern.

```nix
{
  uci.files = [
    {
      path = "/etc/init.d/nft-rules";
      executable = true;
      content = ''
        #!/bin/sh /etc/rc.common
        START=99
        start() { nft -f /etc/nftables.conf; }
      '';
    }
    {
      path = "/etc/nftables.conf";
      content = ''
        table inet filter {
          chain input {
            type filter hook input priority 0; policy drop;
            ct state established,related accept;
          }
        }
      '';
    }
  ];
}
```

## Binary file with checksum-guarded idempotency

```nix
{
  uci.files = [
    {
      path = "/usr/bin/blob";
      # base64-encoded binary content (decoded on-target via base64 -d)
      base64 = "aGVsbG8=";
      executable = true;
      # optional: skip the write when the target hash already matches
      checksum = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    }
  ];
}
```

## Cron via files

```nix
{
  uci.files = [ {
    path = "/etc/crontabs/root";
    content = ''
      0 4 * * * /usr/sbin/logrotate
    '';
  } ];
}
```

## Escape hatch: `rawUci`

```nix
{
  uci.rawUci = [
    "uci rename network.lan=lan0"
    "uci reorder wireless.@wifi-iface[0]=1"
  ];
}
```

## Day-1 firmware build

Combine `nuci` with `nix-openwrt-imagebuilder` to bake a `uci-defaults` script
into a sysupgrade image, so a fresh router boots already configured:

```nix
# flake outputs (perSystem)
exampleFirmware = uci.buildFirmware {
  configuration = ./example.nix;
  profile = "linksys_e8450-ubi";
};
# nix build .#firmware  -> flashable sysupgrade.bin
```

The image builder embeds the compiled `uci` commands as a `uci-defaults` script
that runs on first boot. Pair this with SOPS in **bootstrap mode** (plain
passwords) for the initial image, then switch to `@placeholder@` + SOPS for
subsequent `nuci deploy` runs — exactly the `isBootstrap` pattern in
`example.nix`.
