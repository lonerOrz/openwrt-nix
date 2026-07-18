#!/usr/bin/env python3
"""Realistic OpenWrt test container harness for nuci.

This is the single seam for spinning up lifecycle-real OpenWrt targets:
  * real opkg (22.03.3) or real apk (latest rootfs)
  * real UCI, real dropbear/SSH, real SOPS decryption, real sha512 password hashing
  * per-instance isolation — every Target is a fresh container, torn down explicitly

The only deliberate container concession is /sbin/reload_config: procd is not
running in a rootfs container, so a real reload_config would no-op anyway. nuci's
*fallback* path (per-service init.d reload, discovered at runtime) is what we
exercise through TestSmartReloadFallback — that is the real nuci behaviour.
"""

from __future__ import annotations

import os
import shutil
import socket
import subprocess
import time
import uuid
from dataclasses import dataclass
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
ENGINE = os.environ.get("CONTAINER_ENGINE", "podman")

SESSION_ID = uuid.uuid4().hex[:8]


# ---------------------------------------------------------------------------
# Low-level engine helpers
# ---------------------------------------------------------------------------


def _run(cmd, *, check=True, capture=True, **kw):
    return subprocess.run(cmd, check=check, capture_output=capture, text=True, **kw)


def engine(*args, check=True, **kw):
    return _run([ENGINE, *args], check=check, **kw)


def get_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _build_json(attr: str) -> str:
    return _run(
        [
            "nix",
            "build",
            f"path:{PROJECT_ROOT}#{attr}",
            "--print-out-paths",
            "--no-link",
        ]
    ).stdout.strip()


def wait_for_port(host: str, port: int, timeout: int = 30) -> None:
    for _ in range(timeout):
        try:
            with socket.create_connection((host, port), timeout=1):
                return
        except OSError:
            time.sleep(1)
    raise TimeoutError(f"Port {host}:{port} not reachable after {timeout}s")


# ---------------------------------------------------------------------------
# Shared secrets / ssh / package tooling (built once per session, reused)
# ---------------------------------------------------------------------------


@dataclass
class SessionArtifacts:
    ssh_key: Path
    ssh_config: Path
    sops_key_dir: Path
    sops_pubkey: str
    package_dir: Path
    encrypted_secrets: Path
    opkg_json: str
    apk_json: str


def bootstrap_session() -> SessionArtifacts:
    """Build SSH key, age/SOPS keypair, test packages, encrypted secrets.

    Idempotent within a session: safe to call once and share across targets.
    """
    ssh_key = Path(f"/tmp/nuci_key_{SESSION_ID}")
    ssh_config = Path(f"/tmp/nuci_ssh_config_{SESSION_ID}")
    sops_key_dir = Path(f"/tmp/nuci_sops_{SESSION_ID}")
    package_dir = Path(f"/tmp/nuci_packages_{SESSION_ID}")
    encrypted_secrets = PROJECT_ROOT / "test" / "secrets.enc.json"

    # SSH key
    if not ssh_key.exists():
        _run(
            [
                "ssh-keygen",
                "-t",
                "ed25519",
                "-N",
                "",
                "-f",
                str(ssh_key),
                "-C",
                "openwrt-test",
                "-q",
            ]
        )
    pub_key = (Path(f"{ssh_key}.pub")).read_text().strip()

    # SOPS / age
    sops_key_dir.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["SOPS_AGE_KEY_FILE"] = str(sops_key_dir / "keys.txt")
    if not (sops_key_dir / "keys.txt").exists():
        out = _run(
            ["nix", "shell", "nixpkgs#age", "-c", "age-keygen"],
            env=env,
        ).stdout
        (sops_key_dir / "keys.txt").write_text(out)
    keys_content = (sops_key_dir / "keys.txt").read_text()
    import re

    m = re.search(r"age1[a-z0-9]+", keys_content)
    if not m:
        raise RuntimeError("Failed to extract age public key")
    sops_pubkey = m.group(0)

    # Encrypt mock secrets — always re-encrypt with the current age key, since
    # the source is static and a stale enc file (from a prior session with a
    # different key) would fail to decrypt.
    _run(
        [
            "nix",
            "shell",
            "nixpkgs#sops",
            "-c",
            "sops",
            "--config",
            "/dev/null",
            "--encrypt",
            "--age",
            sops_pubkey,
            "--input-type",
            "json",
            "--output-type",
            "json",
            "--output",
            str(encrypted_secrets),
            str(PROJECT_ROOT / "test" / "mock_secrets" / "secrets.json"),
        ],
        env=env,
    )

    # Test packages (real .ipk / .apk built in-memory)
    package_dir.mkdir(parents=True, exist_ok=True)
    symlink = PROJECT_ROOT / "packages"
    symlink.unlink(missing_ok=True)
    symlink.symlink_to(package_dir)
    _build_test_packages(package_dir)

    # Inject the harness public key into the Nix test configs on disk so that
    # nuci's deploy writes it into the target's authorized_keys (it would
    # otherwise be clobbered by the config-only key list on every deploy).
    for cfg in ["test_config.nix", "test_config_apk.nix"]:
        p = PROJECT_ROOT / "test" / cfg
        new = p.read_text().replace(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleKey test@host", pub_key
        )
        # Replace any pre-existing sshKeys entry so the harness key is the one
        # nuci deploys (the placeholder above may already have been swapped).
        new = _replace_sshkeys(new, pub_key)
        p.write_text(new)

    ssh_config.write_text(_ssh_config_text(ssh_key, 0))  # port patched per target

    # Build the test JSONs AFTER secrets.enc.json is on disk. Nix evaluates
    # `builtins.pathExists ./secrets.enc.json` at build time; if the file is
    # absent on first evaluation the derivation doesn't depend on it and a
    # later-present file is served from cache with empty secrets. Building here
    # (file guaranteed present) forces a correct, secrets-bearing evaluation.
    opkg_json = _build_json("test-json")
    apk_json = _build_json("test-json-apk")

    return SessionArtifacts(
        ssh_key=ssh_key,
        ssh_config=ssh_config,
        sops_key_dir=sops_key_dir,
        sops_pubkey=sops_pubkey,
        package_dir=package_dir,
        encrypted_secrets=encrypted_secrets,
        opkg_json=opkg_json,
        apk_json=apk_json,
    )


def _replace_sshkeys(nix_text: str, pub_key: str) -> str:
    """Rewrite the `uci.sshKeys = [ ... ]` block to contain only pub_key."""
    import re

    pattern = re.compile(r"(uci\.sshKeys\s*=\s*\[)[^\]]*(\])", re.DOTALL)
    return pattern.sub(lambda m: f'{m.group(1)} "{pub_key}" {m.group(2)}', nix_text)


def _ssh_config_text(ssh_key: Path, port: int) -> str:
    return (
        f"Host openwrt-target\n"
        f"    HostName 127.0.0.1\n"
        f"    Port {port}\n"
        f"    User root\n"
        f"    StrictHostKeyChecking no\n"
        f"    UserKnownHostsFile /dev/null\n"
        f"    IdentityFile {ssh_key}\n"
        f"    IdentitiesOnly yes\n"
    )


def _build_test_packages(pkg_dir: Path) -> None:
    # opkg local packages: fetch a real official .ipk from the repo inside a
    # throwaway opkg container (hand-built .ipk is unnecessary — opkg installs
    # compliant real packages fine, and using the real repo keeps the test
    # honest). See _fetch_real_opk.
    _fetch_real_opk(pkg_dir, "tcpdump", "tcpdump.ipk")

    # A real, valid v3 .apk must come from a live apk repo — apk-tools 3.x
    # rejects hand-built packages. Fetch one inside a transient apk container
    # (which has network to downloads.openwrt.org) and copy it out so the apk
    # local-package deploy path is exercised against a genuinely installable
    # package. See _fetch_real_apk.
    _fetch_real_apk(pkg_dir, "libuci20250120", "libuci20250120.apk")


def _fetch_real_opk(pkg_dir: Path, pkg: str, out_name: str) -> None:
    """Pull a real .ipk from the repo inside a throwaway opkg container.

    opkg installs compliant packages regardless of origin, but fetching the
    real package from downloads.openwrt.org keeps the local-package deploy
    path exercised against a genuinely installable artifact.
    """
    import uuid as _uuid

    cname = f"nuci-opkfetch-{_uuid.uuid4().hex[:6]}"
    engine("run", "-d", "--name", cname, "openwrt-test-opkg-env", check=True)
    try:
        engine(
            "exec",
            cname,
            "sh",
            "-c",
            f"opkg update >/dev/null 2>&1; cd /tmp && opkg download {pkg}",
            check=True,
        )
        inside = (
            f"f=$(ls /tmp/{pkg}_*.ipk 2>/dev/null | head -n1); "
            f"[ -n \"$f\" ] && mv \"$f\" /tmp/{out_name} && echo OK"
        )
        renamed = engine("exec", cname, "sh", "-c", inside, check=True).stdout.strip()
        if renamed != "OK":
            raise RuntimeError(f"opkg download produced no {pkg}_*.ipk in {cname}")
        engine("cp", f"{cname}:/tmp/{out_name}", str(pkg_dir / out_name), check=True)
    finally:
        engine("rm", "-f", cname, check=False)


def _fetch_real_apk(pkg_dir: Path, pkg: str, out_name: str) -> None:
    """Pull a real v3 .apk from the repo inside a throwaway apk container.

    apk-tools 3.x only installs valid v3 packages; those can't be produced by
    hand-rolled archives. The only compliant source is the official repo,
    reachable from inside the apk container.
    """
    import uuid as _uuid

    cname = f"nuci-apkfetch-{_uuid.uuid4().hex[:6]}"
    engine("run", "-d", "--name", cname, "openwrt-test-apk-env", check=True)
    try:
        engine(
            "exec",
            cname,
            "sh",
            "-c",
            f"apk update >/dev/null 2>&1; apk fetch -U {pkg}",
            check=True,
        )
        # apk fetch emits a versioned filename (e.g. libuci20250120-..-r1.apk);
        # rename it to the fixed out_name so callers can rely on the path.
        inside = (
            f"f=$(ls {pkg}-*.apk 2>/dev/null | head -n1); "
            f'[ -n "$f" ] && mv "$f" {out_name} && echo OK'
        )
        renamed = engine(
            "exec", cname, "sh", "-c", inside, check=True
        ).stdout.strip()
        if renamed != "OK":
            raise RuntimeError(f"apk fetch produced no {pkg}-*.apk in {cname}")
        engine(
            "cp",
            f"{cname}:{out_name}",
            str(pkg_dir / out_name),
            check=True,
        )
    finally:
        engine("rm", "-f", cname, check=False)


# ---------------------------------------------------------------------------
# Target — one realistic OpenWrt container
# ---------------------------------------------------------------------------


class Target:
    """A lifecycle-real OpenWrt target with real opkg or apk.

    Isolation is per-instance: callers create a Target, use it, then call
    destroy(). No state leaks between targets.
    """

    def __init__(self, role: str, artifacts: SessionArtifacts):
        self.role = role
        self.artifacts = artifacts
        self.name = f"nuci-{role}-{SESSION_ID}-{uuid.uuid4().hex[:4]}"
        self.port = get_free_port()
        self.ssh_config = Path(f"/tmp/nuci_cfg_{self.name}")
        self.ssh_config.write_text(_ssh_config_text(artifacts.ssh_key, self.port))
        self._build_and_start()
        self._inject_ssh_key()

    # -- lifecycle -----------------------------------------------------------

    def _image_and_args(self):
        if self.role == "opkg":
            return (
                "openwrt-test-opkg-env",
                ["-f", str(PROJECT_ROOT / "test" / "Containerfile.opkg")],
            )
        if self.role == "apk":
            return (
                "openwrt-test-apk-env",
                ["-f", str(PROJECT_ROOT / "test" / "Containerfile.apk")],
            )
        if self.role == "agent":
            return (
                "openwrt-agent-test-env",
                ["-f", str(PROJECT_ROOT / "test" / "Containerfile.agent-test")],
            )
        raise ValueError(f"unknown role {self.role}")

    def _build_and_start(self):
        image, args = self._image_and_args()
        engine("build", "-q", "-t", image, *args, str(PROJECT_ROOT))
        caps = ["--cap-add=NET_ADMIN"] if self.role != "agent" else []
        engine(
            "run",
            "-d",
            "--name",
            self.name,
            *caps,
            "-p",
            f"{self.port}:22",
            image,
        )
        wait_for_port("127.0.0.1", self.port)

    def _inject_ssh_key(self):
        if self.role == "agent":
            return  # agent container starts with NO keys by design
        pub_key = (Path(f"{self.artifacts.ssh_key}.pub")).read_text()
        engine(
            "exec",
            "-i",
            self.name,
            "sh",
            "-c",
            "mkdir -p /etc/dropbear && cat > /etc/dropbear/authorized_keys",
            input=pub_key,
        )
        engine("exec", self.name, "chmod", "700", "/etc/dropbear")
        engine("exec", self.name, "chmod", "600", "/etc/dropbear/authorized_keys")

    def destroy(self):
        import glob

        for sock in glob.glob("/tmp/ssh-*"):
            Path(sock).unlink(missing_ok=True)
        engine("rm", "-f", self.name, check=False)
        self.ssh_config.unlink(missing_ok=True)

    # -- exec / ssh ----------------------------------------------------------

    def sh(self, cmd: str, *, check=True) -> str:
        r = engine("exec", self.name, "sh", "-c", cmd, check=check)
        return r.stdout.strip()

    def sh_ok(self, cmd: str) -> bool:
        return engine("exec", self.name, "sh", "-c", cmd, check=False).returncode == 0

    def ssh(self, cmd: str, *, check=True, timeout=10) -> str:
        r = _run(
            [
                "ssh",
                "-o",
                "BatchMode=yes",
                f"-oConnectTimeout={timeout}",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-F",
                str(self.ssh_config),
                "openwrt-target",
                cmd,
            ],
            check=check,
            timeout=timeout + 5,
        )
        return r.stdout.strip()

    def ssh_ok(self, cmd: str, timeout=10) -> bool:
        return (
            _run(
                [
                    "ssh",
                    "-o",
                    "BatchMode=yes",
                    f"-oConnectTimeout={timeout}",
                    "-o",
                    "StrictHostKeyChecking=no",
                    "-o",
                    "UserKnownHostsFile=/dev/null",
                    "-F",
                    str(self.ssh_config),
                    "openwrt-target",
                    cmd,
                ],
                check=False,
                timeout=timeout + 5,
            ).returncode
            == 0
        )

    # -- uci helpers ---------------------------------------------------------

    def uci_get(self, path: str) -> str | None:
        r = engine("exec", self.name, "uci", "get", path, check=False)
        return r.stdout.strip() if r.returncode == 0 else None

    def uci_exists(self, path: str) -> bool:
        return (
            engine("exec", self.name, "uci", "get", path, check=False).returncode == 0
        )

    def uci_set(self, path: str, value: str):
        self.sh(f"uci set {path}='{value}' && uci commit")

    # -- sops env ------------------------------------------------------------

    @property
    def sops_env(self) -> dict:
        env = os.environ.copy()
        env["SOPS_AGE_KEY_FILE"] = str(self.artifacts.sops_key_dir / "keys.txt")
        env["NUCI_WATCHDOG_TIMEOUT"] = "10"
        return env

    # -- nuci binary helpers -------------------------------------------------

    def nuci(self, *args, timeout=120) -> subprocess.CompletedProcess:
        return _run(
            [
                "cargo",
                "run",
                "--",
                *args,
                "--target",
                "root@127.0.0.1",
                "--port",
                str(self.port),
                "--identity",
                str(self.artifacts.ssh_key),
            ],
            check=False,
            env=self.sops_env,
            cwd=str(PROJECT_ROOT),
            timeout=timeout,
        )

    def wait_reconnect(self, tries=30) -> bool:
        for _ in range(tries):
            time.sleep(2)
            if self.ssh_ok("echo ok", timeout=3):
                return True
        return False


def cleanup_session(artifacts: SessionArtifacts):
    import glob

    for sock in glob.glob("/tmp/ssh-*"):
        Path(sock).unlink(missing_ok=True)
    for n in [
        artifacts.ssh_key,
        Path(f"{artifacts.ssh_key}.pub"),
        artifacts.ssh_config,
    ]:
        n.unlink(missing_ok=True)
    shutil.rmtree(artifacts.sops_key_dir, ignore_errors=True)
    shutil.rmtree(artifacts.package_dir, ignore_errors=True)
    (PROJECT_ROOT / "packages").unlink(missing_ok=True)
    artifacts.encrypted_secrets.unlink(missing_ok=True)
    _run(["git", "restore", "--staged", str(artifacts.encrypted_secrets)], check=False)
    _run(
        ["git", "restore", "test/test_config.nix", "test/test_config_apk.nix"],
        check=False,
    )
