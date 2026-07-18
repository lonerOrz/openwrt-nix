# nuci

Declarative OpenWrt config: write it in Nix, compile to UCI with Rust, deploy over SSH.

```
Nix ──► uci.json ──► nuci ──► SSH ──► Router
```

The router only runs a small shell script. All the thinking — validation,
secret decryption, UCI serialization — happens on your machine.

## Why

- LUCI config isn't version-controlled or reproducible.
- Ansible on a 128MB router is miserable.
- You can't run Nix on the router itself.

nuci keeps your config in Nix and ships a plain `uci batch` script to the device.

## Workflow

1. Write Nix config; mark secrets with `@placeholder@`.
2. `nuci compile` — validates, decrypts SOPS, prints a `uci batch` script.
3. `nuci diff --target root@router` — read-only preview of changes and which services reload.
4. `nuci deploy --target root@router` — pipes the script over SSH, with rollback safety.

```bash
just apply       # full deploy to $ROUTER_HOST
just dry-run     # diff only
```

## How a deploy stays safe

Before touching anything, nuci saves `/etc/.uci-rollback-backup` and installs a
self-deleting boot hook (`/etc/init.d/nuci_rollback`). If the connection drops:

- **Within ~60s** — a watchdog restores the backup and reloads services.
- **On reboot** — the boot hook restores the backup on next start, then removes itself.

No manual recovery needed either way.

## Service reloads

Rather than hardcode which init script owns each config, nuci discovers it on
the target at deploy time:

1. `/etc/init.d/<config>` exists → reload it.
2. `wireless` → `/sbin/wifi reload` (or `network restart`).
3. Otherwise it greps `config_load <config>` in `/etc/init.d/*` as a best-effort
   guess for arbitrary services. The canonical OpenWrt reload path is
   `reload_config`/procd, which nuci uses when available; this grep is a known
   heuristic, not an official API.

Reloads are targeted — only services tied to changed configs restart, not the
whole box.

## Declarative ownership

nuci describes the **end state**, not a diff:

- **Named sections** (`network.lan`): left alone unless you declare and later
  remove them in Nix. Safe to edit by hand.
- **Anonymous sections** (`system.@system[0]`): fully rebuilt on each deploy.
  nuci clears every anonymous section of a type it owns and re-adds yours.

So: don't hand-add anonymous sections of a type nuci manages — they get wiped.

## CLI

```
nuci compile <json> [secrets_dir] [--no-sops]
nuci deploy <json> --target <user@host> [--port PORT] [--identity FILE] [--force]
nuci diff   <json> --target <user@host> [--port PORT] [--identity FILE]
```

`--force` skips the idempotency check and applies regardless.

## Testing

```bash
just test-unit          # cargo unit tests
just test-integration   # real OpenWrt containers via Podman
just test-all           # both
```

Integration tests spin up isolated real OpenWrt containers (opkg 23.05.5 and
apk latest) — no physical router required. They cover compile output, real
deploys, idempotent list ordering, section deletion, diff accuracy, SSH-key
lockout prevention, watchdog rollback, and targeted service reloads.

## Layout

```
src/            Rust: CLI, compile pipeline, deploy, diff, UCI serialization, validation, secrets
test/           pytest integration suite + container harness + real OpenWrt Containerfiles
nix/            Nix module options, uci writer, firmware + package builds
```

## License

MIT
