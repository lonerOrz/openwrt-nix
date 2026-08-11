# nuci

Declarative OpenWrt configuration — write in Nix, compile with Rust, deploy over SSH.

```text
Nix (writeUci) ──► uci.json ──► nuci compile/diff/deploy ──► SSH ──► Router
```

## Features

- **Declarative UCI**: Idempotent named (delete+set) and anonymous (while delete) section rebuilding.
- **Package Management**: opkg & apk backends. Package removals (`-pkg`) execute before installs.
- **SOPS Secrets**: In-memory age decryption at compile time (`@placeholder@`). `--no-sops` pure compiler mode.
- **Arbitrary Files**: Text via POSIX cat heredocs, binary via base64 `-d`, SHA256 checksum idempotency.
- **Safety Net**: 60s watchdog (`trap '' HUP`) + self-deleting `S15nuci_rollback` boot hook.
- **Async Reload**: Background subshell `(sleep 1; reload) &` prevents SSH disconnects (exit status 255).
- **Lockout Prevention**: Auto-appends active deployer's SSH agent key if missing.

## Quick Start

```bash
nuci diff   ./uci.json --target root@router          # Read-only diff
nuci deploy ./uci.json --target root@router --force  # Deploy with rollback net
nix run .#example -- "root@router"                   # Flake one-shot deploy
```

## Documentation

- [Index](docs/index.md) — Overview & quick start
- [Architecture](docs/arch.md) — Domain layout, UCI idempotency, safety net, async reload
- [Nix Options](docs/nix-options.md) — Exact Nix module option specifications
- [Examples](docs/examples.md) — Copy-paste configuration snippets

## License

MIT
