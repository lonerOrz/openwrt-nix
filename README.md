# nuci

Declarative OpenWrt config: write it in Nix, compile to UCI with Rust, deploy over SSH.

```
Nix ──► uci.json ──► nuci ──► SSH ──► Router
```

The router only runs a small shell script. All the thinking — validation,
secret decryption, UCI serialization — happens on your machine.

## Why

- LuCI config isn't version-controlled or reproducible.
- Ansible on a 128MB router is miserable.
- You can't run Nix on the router itself.

## Quick start

```bash
nuci diff   ./uci.json --target root@router   # read-only preview
nuci deploy ./uci.json --target root@router --force
```

## Docs

Full architecture, design philosophy, and copy-paste Nix examples live on the
**documentation site** (GitHub Pages):

👉 https://lonerOrz.github.io/openwrt-nix/

Source for the docs is in [`docs/`](docs/).

## Testing

```bash
just test-unit          # cargo unit tests
just test-integration   # real OpenWrt containers via Podman
just test-all           # both
```

## License

MIT
