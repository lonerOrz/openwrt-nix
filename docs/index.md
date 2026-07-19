# nuci — Declarative OpenWrt Configuration

> Write your router config in Nix, compile it to UCI with Rust, and deploy it over SSH — idempotently, safely, and with a built-in anti-brick safety net.

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

## Why nuci?

Managing OpenWrt configurations has historically meant one of three painful options:

| Approach                 | Pain point                                                                |
| ------------------------ | ------------------------------------------------------------------------- |
| **LuCI** (web UI)        | No version control, no review, click-ops, diverges silently from reality. |
| **Ansible / generic CM** | Too heavy for 128 MB routers; shell-driven, rarely idempotent on UCI.     |
| **Pure NixOS**           | NixOS does not run on OpenWrt; the router's userspace is busybox + procd. |

`nuci` sits in the gap: you keep **Nix** as the source of truth (typed, reviewable,
reproducible), and `nuci` compiles that to the UCI directives OpenWrt actually speaks,
then deploys them over SSH with the same guarantees you expect from a real config
management tool — idempotency, diff preview, and a two-layer rollback safety net.

## What's in the docs

- **[Architecture](architecture.md)** — how nuci achieves idempotency, safety, and smart reloads under the hood.
- **[Features & Design Philosophy](features.md)** — what nuci covers, what it deliberately doesn't, and the escape hatches for everything else.
- **[Nix Examples](examples.md)** — copy-paste configuration snippets for real-world setups.

## Quick start

```bash
# Compile a Nix config to the intermediate JSON nuci understands
nix run .#example-json

# Preview what *would* change on the live router (read-only)
nuci diff ./uci.json --target root@192.168.1.1

# Deploy (idempotent; rolls back automatically on failure)
nuci deploy ./uci.json --target root@192.168.1.1 --force
```

Or drive the whole flow from a Nix flake — compile and deploy in one command:

```bash
# Full deploy to $ROUTER_HOST (set ROUTER_HOST or pass the target)
nix run .#example -- "root@192.168.1.1"
```
