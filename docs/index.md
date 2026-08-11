# nuci

Declarative OpenWrt UCI configuration compiler & SSH deployer.

```text
Nix (writeUci) ──► uci.json ──► nuci compile ──► UCI Batch
                                  │
                                  ├─► nuci diff   (read-only)
                                  └─► nuci deploy ──► SSH ──► Router
```

## CLI Quick Reference

```bash
nuci compile ./uci.json --no-sops                    # Pure UCI compilation
nuci diff    ./uci.json --target root@192.168.1.1    # Read-only diff
nuci deploy  ./uci.json --target root@192.168.1.1    # Deploy with 60s watchdog
nix run .#example -- "root@192.168.1.1" --force      # Flake one-shot deploy
```

## Core Modules

- **Architecture** — 4-domain code layout, UCI idempotency, watchdog + boot hook, async reload.
- **Nix Options** — Complete option specification generated from `nix/module-options.nix`.
- **Examples** — Copy-paste Nix snippets.
