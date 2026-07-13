# nuci

Declarative config management for OpenWrt. Define everything in Nix, compile with Rust, deploy over SSH.

```
Nix ──► uci.json ──► nuci ──► SSH pipe ──► Router
                         │
                    validates, resolves
                    secrets, serializes
                    UCI batch commands
```

## Why?

- Web UI config = no version control, no reproducibility
- Ansible on a 128MB router = painful
- Running Nix on the router itself = impossible

**nuci** does all the heavy lifting locally. The router just runs a small shell script.

## How it works

1. You write Nix config with `@placeholder@` for secrets
2. `nuci compile` → validates, decrypts SOPS, outputs `uci batch` script
3. `nuci diff` → read-only preview of what would change, including which services would be reloaded
4. `nuci deploy` → SSHes the script to the router, with rollback safety

### Deploy pipeline

```
1. Compile Nix → UCI batch        (local)
2. Idempotency check              (SSH read-only — skip if unchanged)
3. Service discovery               (scan target's /etc/init.d/* for config_load bindings)
4. Transfer local packages         (tar-over-SSH stdin, no SCP dependency)
5. Persistent backup + watchdog    (/etc/.uci-rollback-backup + boot hook)
6. Apply UCI changes               (targeted reload, not blanket restart)
7. Confirm connectivity            (cancel watchdog on success)
```

### Rollback & self-healing

Before applying changes, nuci saves a persistent backup to `/etc/.uci-rollback-backup` and installs a self-destructing boot hook (`/etc/init.d/nuci_rollback`). If SSH dies mid-deploy:

- **Within 60s**: the watchdog timer fires, restores the backup, and reloads services.
- **Power cycle**: the boot hook runs on next startup, restores the backup, and deletes itself.

Either way, the router recovers without manual intervention.

### Dynamic service reload

Instead of hardcoding which init.d script handles each UCI config, nuci scans the target device at deploy time:

1. `/etc/init.d/<config>` exists → use it directly
2. Special case: `wireless` → `/sbin/wifi reload` (non-destructive)
3. Generic: `grep config_load <name> /etc/init.d/*` → finds the right script (e.g. `dhcp` → `dnsmasq`)

This means custom services and non-standard OpenWrt variants work out of the box.

## Quick start

```bash
# Setup
age-keygen -o age.key
sops test/secrets.enc.json     # add wifi_password, root_password, etc.

# Edit config
vim example.nix                 # use @wifi_password@ for secrets

# Deploy
export ROUTER_HOST=192.168.1.1
just apply                      # full deploy
just dry-run                    # preview diff only
```

## CLI

```
nuci compile <json> [secrets_dir]
    Compile Nix JSON config into UCI batch commands (stdout).

nuci deploy <json> --target <user@host> [options]
    --port <port>                 SSH port (default: 22)
    --identity <key_file>         SSH identity file
    --secrets_dir <dir>           Directory containing SOPS secrets
    --force                       Skip idempotency check, deploy even if unchanged

nuci diff <json> --target <user@host> [options]
    --port <port>                 SSH port (default: 22)
    --identity <key_file>         SSH identity file
    --secrets_dir <dir>           Directory containing SOPS secrets

    Shows colored diff of UCI state + lists which services would be
    reloaded (auto-discovered from the target's init.d scripts).
```

## Testing

```bash
just test-all      # 88 unit + 28 integration = 116 tests
just test-unit     # cargo test + mock JSON
```

Integration tests run real OpenWrt containers via Podman — no physical router needed.

| Test class                  | What it verifies                                  |
| --------------------------- | ------------------------------------------------- |
| `TestCommandGeneration`     | UCI batch syntax (opkg + apk)                     |
| `TestDeployment`            | Deploy + UCI state verification                   |
| `TestWatchdogRollback`      | Auto-restore after config break                   |
| `TestPersistentWatchdog`    | Power-cycle recovery via boot hook                |
| `TestNetworkFaultInjection` | Watchdog under packet loss / blackout             |
| `TestAgentLockout`          | SSH key lockout prevention                        |
| `TestRealDeploy`            | End-to-end `nuci deploy` + `nuci diff` binary     |
| `TestSmartReloadFallback`   | Targeted reload when `/sbin/reload_config` absent |

Each test run gets unique UUID-based container names and dynamic ports — run multiple suites in parallel safely.

## Structure

```
src/
├── main.rs          # CLI (clap), compile, deploy, diff
├── pipeline.rs      # Shared compile pipeline (parse → validate → decrypt → resolve)
├── deploy.rs        # SSH transport, tar-over-SSH, watchdog, service discovery
├── diff.rs          # Read-only diff + dynamic service scanning
├── generator.rs     # UCI batch serialization
├── validation.rs    # UCI spec validation
├── secrets.rs       # SOPS decryption + @placeholder@ resolution
├── models.rs        # JSON config models
├── helpers.rs       # Utilities
└── error.rs         # Structured error enum (Io, Json, Validation, Sops, Deploy)

test/
├── integration_test.py      # 28 pytest tests (UUID isolation, dynamic ports)
├── package-server.py        # .ipk/.apk builder
├── Containerfile            # OpenWrt sandbox
└── test_config.nix          # Test fixture

nix/
├── default.nix      # writeUci + deployment script
├── module-options.nix
└── nuci.nix         # Rust package build
```

## License

MIT
