# nuci

**Declarative OpenWrt configuration** — write it in Nix, compile to UCI with
Rust, and deploy it over SSH.

```text
  Nix module (writeUci)
        │  eval: validate, decrypt SOPS, serialize UCI
        ▼
     uci.json  ──────────────┐
        │                    │
        ▼                    ▼
   nuci compile        nuci diff (read-only preview)
        │                    │
        ▼                    ▼
   uci batch script ──►  nuci deploy ──► SSH ──► Router
                              │
                              ├─ snapshot /etc/config  (rollback backup)
                              ├─ apply uci batch + files + packages
                              ├─ smart service reload (procd / init.d)
                              └─ watchdog + boot hook (anti-brick)
```

The router only runs a small shell script. All the thinking — validation,
secret decryption, UCI serialization — happens on your machine.

## Why nuci?

| Alternative       | Problem                                                   |
| ----------------- | --------------------------------------------------------- |
| LuCI (web UI)     | No version control, no review, config drifts silently.    |
| Ansible / generic | Too heavy for a 128 MB router; rarely idempotent on UCI.  |
| Pure NixOS        | NixOS does not run on OpenWrt — the userspace is busybox. |

`nuci` keeps your config in Nix (typed, reviewable, reproducible) and ships a
plain `uci batch` script to the device.

## Features

- **Declarative UCI** — scalar/list options, named & anonymous sections, fully
  rebuilt idempotently so removed options are removed on the router.
- **Package management** — opkg / apk backends, custom feeds, local `.ipk`/`.apk`.
- **Secrets** — SOPS + age decryption at compile time (`@placeholder@` syntax).
- **Arbitrary files** — write any file via `files`, including binary (base64)
  and checksum-guarded idempotent writes.
- **Safety net** — rollback watchdog + self-deleting boot hook prevent bricking.
- **Escape hatch** — `rawUci` for directives the typed model can't express.

## Quick start

```bash
nuci diff   ./uci.json --target root@router   # read-only preview
nuci deploy ./uci.json --target root@router --force
```

Or via the Nix flake:

```bash
nix run .#example -- "root@router"   # full deploy to $ROUTER_HOST
```

## Documentation

Full architecture, design philosophy, and copy-paste Nix examples are on the
documentation site:

> https://lonerOrz.github.io/openwrt-nix/

The source for the docs lives in [`docs/`](docs/).

## Testing

```bash
just test-unit          # cargo unit tests
just test-integration   # real OpenWrt containers (Podman)
just test-all           # both
```

## License

MIT
