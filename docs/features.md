# Features & Design Philosophy

`nuci` covers the **core 80%** of router configuration declaratively, and
provides **escape hatches** for the other 20% rather than pretending to model
every third-party package.

## What nuci does declaratively

### Declarative UCI

- **Scalar** and **list** options (`ports = [ "lan1" "lan2" ]`).
- **Named sections** (`network.lan = { _type = "interface"; ... }`).
- **Anonymous list sections** (`wireless.@wifi-iface[0]`), rebuilt idempotently.
- Full rebuild strategy means removing an option from Nix removes it on the
  router — no silent drift.

### Package management (opkg + apk)

- Dual backend: `packageManager = "opkg"` or `"apk"`.
- `packages`: install from the repo.
- `packageSources.feeds`: inject custom opkg feeds.
- `packageSources.localPackages`: ship real `.ipk` / `.apk` files. For opkg the
  install is guarded by `opkg list-installed <name>`; for apk the file is
  installed directly with `apk add --allow-untrusted` (apk filenames don't
  reliably encode the package name).

> **Ordering pitfall (solved):** custom feeds are written _before_ package
> installs, and official packages are installed _before_ custom-source packages,
> so a package that depends on a feed never hits a dead-link error.

### Secrets (SOPS)

- `secrets.sops.files = [ ./secrets.yml ]` decrypts with `sops` + age at compile
  time.
- Placeholders in config use `@name@` syntax (e.g.
  `key = "@wifi_password@"`). Missing placeholders are a **compile error**, not a
  silent blank.

### SSH keys & lockout prevention

- `sshKeys` writes authorized keys.
- The deployer's own key is **auto-appended** if absent, so a config mistake
  can't lock you out of the box you're deploying from.

### Arbitrary files (`files`)

- Write any file to any absolute path: configs for non-UCI apps, init scripts,
  crontabs.
- `executable = true` → mode `0755`, else `0644`.
- **Binary content:** `content = { "base64": "..." }` is decoded on-target via
  `base64 -d`.
- **Checksum-guarded idempotency:** an optional `checksum` (sha256 hex) wraps
  the write in `if [ "$(sha256sum path)" != <sum> ]; then ... fi`, so an
  unchanged file is skipped on redeploy.

### Raw UCI escape hatch (`rawUci`)

- For directives the typed `Section` model can't express (`uci rename`,
  `uci reorder`, deleting a single option, exotic types).
- Each entry must be a complete `uci ...` command — validated to start with
  `uci `. This is the one place raw shell reaches the target, and it's
  deliberately constrained.

### Diff preview

- `nuci diff` is read-only: it fetches live router state, compares it to the
  Nix model, and prints a colored preview of UCI changes, package installs, and
  auto-discovered affected services.

## Design philosophy: 80/20 + escape hatches

### Why not 100% coverage?

Writing a typed Rust `Section` schema for every OpenWrt package (AdGuardHome,
sqm, bansui, …) is open-ended busywork. `nuci` owns the core surface
(network / wireless / system / firewall / packages / files) and treats
everything else as **data**, not schema.

### Non-UCI apps → validate on the Nix side, write via `files`

For an app configured by YAML/JSON (e.g. AdGuardHome), **do not** teach `nuci`
a schema. Instead:

1. Generate and **validate** the config in Nix using `pkgs.formats.yaml` (or
   `json`) at eval time, so typos fail fast in CI.
2. Hand the rendered string to `nuci` via `files` to drop it on the router.

`nuci` stays a dumb, reliable file writer; Nix stays the type-checker.

```nix
{ pkgs, ... }:
let
  adguardCfg = pkgs.formats.yaml { }.generate "adguard.yaml" {
    bind_host = "192.168.1.1";
    bind_port = 53;
    # ... validated by the yaml format at eval time
  };
in {
  uci.files = [ {
    path = "/etc/adguardhome.yaml";
    # read the Nix-generated file's contents
    content = builtins.readFile adguardCfg;
  } ];
}
```

### Advanced network rules (nftables) → init.d pattern

Don't reach for cron hacks. OpenWrt already runs `/etc/init.d/*` on boot and
network events. Write an init script via `files` and let procd trigger it:

```nix
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
        chain input { type filter hook input priority 0; policy drop; }
      }
    '';
  }
];
```

This uses OpenWrt's **native** lifecycle — cleaner than cronning a reload.

### Cron → overwrite the crontab

Cron on OpenWrt lives in `/etc/crontabs/root`. Just write it:

```nix
uci.files = [ {
  path = "/etc/crontabs/root";
  content = ''
    0 4 * * * /usr/sbin/logrotate
  '';
} ];
```

`nuci diff` will show the text delta directly, so changes are reviewable.

### The escape hatch of last resort: `rawUci`

When you genuinely need a `uci rename` or list reorder that the typed model
can't express, drop to `rawUci`:

```nix
uci.rawUci = [
  "uci rename network.lan=lan0"
  "uci reorder wireless.@wifi-iface[0]=1"
];
```

Every line is validated to begin with `uci ` — this is the single, auditable
place raw commands reach the target.

## What nuci deliberately does NOT do

- **Run a full Nix daemon on the router.** OpenWrt's userspace is busybox +
  procd; Nix stays on the build host.
- **Model every package's schema.** Covered above — use `files` + Nix-side
  validation.
- **Edit `/etc/config/*` files by string patching.** All UCI goes through
  `uci batch` for correctness.
