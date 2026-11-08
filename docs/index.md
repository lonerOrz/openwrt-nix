# nuci

nuci is a declarative OpenWrt UCI configuration compiler and SSH deployer that describes configuration in Nix and safely pushes it to the target device.

```text
Nix config ──► uci.json ──► nuci CLI ──► SSH ──► OpenWrt target
```

## CLI Quick Reference

```bash
nuci compile ./uci.json --no-sops                    # Compile UCI only
nuci diff    ./uci.json --target root@192.168.1.1    # Read-only diff
nuci deploy  ./uci.json --target root@192.168.1.1    # Deploy with 60s watchdog
nix run .#example -- "root@192.168.1.1" --force      # Flake one-shot deploy
```

## Documentation

- [Architecture](arch.md) — components, deployment lifecycle, idempotency, rollback.
- [Nix Module Options](nix-options.md) — full option specification.
- [Examples](example.md) — copy-paste Nix snippets.
