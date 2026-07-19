# Architecture & Internals

How `nuci` achieves **idempotent**, **safe**, **reload-aware** deploys on a
real OpenWrt device.

## Repository layout

```text
src/            Rust core (compile + deploy + diff)
  models.rs     JSON models (Root, Section, File, FileContent, ...)
  pipeline.rs   single compile seam: JSON -> uci batch + side effects
  generator.rs  UCI serialization, package install blocks, feeds
  deploy.rs     SSH orchestration, file writes, watchdog, boot hook
  diff.rs       read-only state comparison + service discovery
  validation.rs config validation (paths, identifiers, content)
  secrets.rs    SOPS decryption + @placeholder@ interpolation
nix/            Nix module (writeUci), firmware image builder
test/           Podman-based real-container integration tests
```

The whole compile path funnels through **one seam** (`pipeline::compile_config`),
so every output — UCI batch, package installs, file writes, raw UCI escape
lines — is derived from a single `Root` model. That is what keeps the system
idempotent: exactly one function decides "what the router should look like".

## UCI serialization & absolute idempotency

OpenWrt's `uci` stores config in `/etc/config/*`. `nuci` never edits those
files directly; it emits `uci batch` directives that set the _desired_ state.
UCI has two kinds of sections, needing different reconciliation strategies.

**Named sections** (`network.lan`) are addressed by a stable name. To avoid
stale options lingering when you remove them from Nix, `nuci` emits:

```sh
uci delete network.lan
uci set network.lan=interface
uci set network.lan.proto='dhcp'
# ...
uci commit network
```

The `delete` wipes the section, then `set` rebuilds it from the Nix model.
Result: removing an option from Nix **removes it from the router** — no drift.

**Anonymous list sections** (`wireless.@wifi-iface[0]`) have **no stable
name**, so you cannot address "the third one" reliably across redeploys.
`nuci` clears them by repeatedly deleting the head until the list is empty,
then re-adds from the model:

```sh
while uci -q delete wireless.@wifi-iface[0]; do :; done
uci add wireless wifi-iface
uci set wireless.@wifi-iface[-1].ssid='gchq-2.4'
# ...
```

This is a deliberate full-rebuild strategy: anonymous sections have no stable
identity to match against, so "clear and rebuild" is the only correct
idempotent move. (This is also why the audit's "stable identity for anon
sections" candidate was **skipped** — full rebuild already guarantees orphan
cleanup.)

**List-order independence.** `uci show` emits list values as separate
`option[0]=`, `option[1]=` lines, and the _order_ you append them can differ
from the router even when the set is identical — causing false "changed"
reports. `nuci` joins list elements with the **Unit Separator** control
character (`\u{1f}`, `LIST_SEP` in `diff.rs`) before comparing, so a reordered
list is recognised as **no change**. The separator is stripped back to a
human-readable `a, b, c` form only for display in `nuci diff`.

```rust
const LIST_SEP: &str = "\u{1f}";
// comparison form:  "a\u{1f}b\u{1f}c"
// display form:     "a, b, c"
```

## The anti-brick safety net

A bad network deploy can lock you out of a router 500 km away. `nuci` ships a
**two-layer** rollback net.

**Layer A — Watchdog (60s, in-session).** Before applying changes, `nuci`
snapshots `/etc/config` to `/etc/.uci-rollback-backup` and spawns a background
watchdog:

```sh
( trap '' HUP; sleep 60; \
  if [ -d /etc/.uci-rollback-backup ]; then \
    cp -a /etc/.uci-rollback-backup/* /etc/config/; \
    /sbin/reload_config; fi; \
  rm -rf /etc/.uci-rollback-backup ... ) &
```

If the deploy succeeds, `nuci` reconnects and **kills the watchdog PID** — the
backup is discarded and the new config stays. If the connection drops (wrong
IP, dead interface, reboot), the watchdog fires after 60 s, restores the
backup, and reloads. The timeout is overridable via `NUCI_WATCHDOG_TIMEOUT`.

**Layer B — Boot-time hook (power loss during deploy).** The watchdog only
helps if the box stays up. If it **reboots mid-deploy** (power loss), `nuci`
installs a procd-style init script `S15nuci_rollback` that, on next boot,
restores `/etc/config` from the backup and then **deletes itself**:

```sh
cat > /etc/init.d/nuci_rollback <<'EOF'
START=15
start() {
  if [ -d /etc/.uci-rollback-backup ]; then
    cp -a /etc/.uci-rollback-backup/* /etc/config/
    rm -rf /etc/.uci-rollback-backup
  fi
  rm -f /etc/init.d/nuci_rollback /etc/rc.d/S15nuci_rollback
}
EOF
```

This is unit-tested in isolation (`boot_rollback_hook_restores_and_self_deletes`)
by rewriting `/etc` to a temp root and asserting the config is restored and the
hook self-deletes. Together, the two layers cover transient network failure
(A) **and** hard power loss (B).

## Smart service reload

After UCI changes, `nuci` must tell services to pick them up. It does **not**
hardcode a service list.

1. **Primary:** it prefers procd's native `/sbin/reload_config`, which reloads
   every service whose config changed.
2. **Fallback:** when procd isn't PID 1 (a container, or minimal setups),
   `nuci` runs a discovery command that greps `/etc/init.d/*` for the official
   `config_load <config>` directive to learn **which services own which
   config**, then reloads only the affected ones (`/etc/init.d/<svc> reload`).
   This heuristic is documented in `diff.rs` as the upstream OpenWrt
   convention — no behaviour change, just an officially-cited fallback.

`nuci diff` prints the discovered affected services, making the reload preview
explicit:

```text
Affected services (auto-discovered):
  dhcp → /etc/init.d/dnsmasq reload
  network → /etc/init.d/network reload
```

## Orphan deletion

Deleting a **named** section from your Nix config causes `nuci` to emit
`uci delete config.section` for the now-absent section (see
`deploy::orphan_delete_commands`). Anonymous-list orphans are handled by the
full-rebuild strategy above. Named-section orphans are the only ones with a
stable key to target, and `nuci` targets exactly them — so the router converges
to the Nix model with no leftovers.

## Summary

| Concern                     | Mechanism                                               |
| --------------------------- | ------------------------------------------------------- |
| Idempotent named sections   | `delete` + `set` rebuild                                |
| Idempotent anon lists       | `while delete @type[0]` + re-add                        |
| No false diffs on reorder   | `LIST_SEP` (`\u{1f}`) comparison                        |
| Lockout safety (network)    | 60 s rollback watchdog (`NUCI_WATCHDOG_TIMEOUT`)        |
| Lockout safety (power loss) | self-deleting `S15nuci_rollback` boot hook              |
| Reload correctness          | procd `reload_config`, else `config_load` grep fallback |
| Stale cleanup               | named-section `uci delete` + anon full-rebuild          |
