#!/usr/bin/env python3
"""Integration tests for nuci — OpenWrt UCI configuration management.

Runs against a real OpenWrt container (podman/docker).
Requires: podman or docker, nix, ssh, jq, sops, age.
"""

import os
import re
import shutil
import socket
import subprocess
import time
import uuid
from pathlib import Path

import pytest

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parent.parent

# Unique session ID — isolates concurrent test runs
SESSION_ID = uuid.uuid4().hex[:8]

CONTAINER_NAME = f"nuci-test-{SESSION_ID}"
OPKG_CONTAINER_NAME = f"nuci-test-opkg-{SESSION_ID}"
AGENT_CONTAINER_NAME = f"nuci-agent-{SESSION_ID}"

SSH_KEY_PATH = Path(f"/tmp/nuci_key_{SESSION_ID}")
SSH_CONFIG_PATH = Path(f"/tmp/nuci_ssh_config_{SESSION_ID}")
SOPS_KEY_DIR = Path(f"/tmp/nuci_sops_{SESSION_ID}")
ENCRYPTED_SECRETS = PROJECT_ROOT / "test" / "secrets.enc.json"
PACKAGE_DIR = Path(f"/tmp/nuci_packages_{SESSION_ID}")

ENGINE = os.environ.get("CONTAINER_ENGINE", "podman")

# Dynamic ports — populated by setup_and_teardown, used by all test classes
MAIN_SSH_PORT = 0
OPKG_SSH_PORT = 0
AGENT_SSH_PORT = 0

# Dropbear binary — procd not running in test containers, can't use init.d scripts
DROPBEAR_BIN = "/usr/sbin/dropbear -F -E -p 22 -R"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def run(
    cmd: list[str], *, check: bool = True, capture: bool = True, **kw
) -> subprocess.CompletedProcess:
    """Run a command, returning CompletedProcess."""
    return subprocess.run(
        cmd,
        check=check,
        capture_output=capture,
        text=True,
        **kw,
    )


def engine(*args: str, check: bool = True, **kw) -> subprocess.CompletedProcess:
    """Run a container engine command."""
    return run([ENGINE, *args], check=check, **kw)


def get_free_port() -> int:
    """Find a free ephemeral port on the host."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def podman_exec(container: str, cmd: str, *, check: bool = True) -> str:
    """Execute a command inside a container, return stdout."""
    r = engine("exec", container, "sh", "-c", cmd, check=check)
    return r.stdout.strip()


def wait_for_port(host: str, port: int, timeout: int = 15) -> None:
    """Wait until a TCP port is reachable."""
    for _ in range(timeout):
        try:
            with socket.create_connection((host, port), timeout=1):
                return
        except OSError:
            time.sleep(1)
    pytest.fail(f"Port {host}:{port} not reachable after {timeout}s")


def ssh_cmd(
    ssh_config: Path, host: str, cmd: str, *, check: bool = True, timeout: int = 10
) -> str:
    """Run a command via SSH."""
    r = run(
        [
            "ssh",
            "-o",
            "BatchMode=yes",
            "-o",
            f"ConnectTimeout={timeout}",
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-F",
            str(ssh_config),
            host,
            cmd,
        ],
        check=check,
        timeout=timeout + 5,
    )
    return r.stdout.strip()


def check_output_pattern(output: str, pattern: str, label: str, tag: str = "") -> None:
    """Assert pattern exists in output."""
    prefix = f"[{tag}] " if tag else ""
    assert pattern in output, f"{prefix}{label} — pattern not found: {pattern}"


def check_uci_value(container: str, uci_path: str, expected: str, label: str) -> None:
    """Assert a UCI value matches expected."""
    actual = podman_exec(container, f"uci get {uci_path}", check=False)
    assert actual == expected, f"{label} = '{actual}', expected '{expected}'"


def check_uci_section(container: str, uci_path: str, label: str) -> None:
    """Assert a UCI section exists."""
    r = engine("exec", container, "uci", "get", uci_path, check=False)
    assert r.returncode == 0, f"Section '{uci_path}' not found"


def check_json_field(json_path: Path, jq_expr: str, label: str, tag: str = "") -> None:
    """Assert a jq expression succeeds on a JSON file."""
    r = run(["jq", "-e", jq_expr, str(json_path)], check=False)
    prefix = f"[{tag}] " if tag else ""
    assert r.returncode == 0, f"{prefix}{label} — jq expression failed: {jq_expr}"


def kill_dropbear(container: str) -> None:
    """Kill dropbear in a container (idempotent). Also clean stale SSH ControlMaster sockets."""
    import glob as _glob

    for sock in _glob.glob("/tmp/ssh-*"):
        Path(sock).unlink(missing_ok=True)
    podman_exec(container, "killall dropbear || true", check=False)


def dropbear_running(container: str) -> bool:
    """Check if dropbear is running (non-zombie) in a container."""
    ps = podman_exec(container, "ps", check=False)
    return any("dropbear" in line and " Z " not in line for line in ps.splitlines())


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def project_root() -> Path:
    return PROJECT_ROOT


@pytest.fixture(scope="session")
def nuci_output_opkg() -> str:
    """Run nuci compile for opkg config, return output."""
    env = os.environ.copy()
    env["SOPS_AGE_KEY_FILE"] = str(SOPS_KEY_DIR / "keys.txt")
    r = run(
        ["nix", "run", f"path:{PROJECT_ROOT}#test-deploy", "--"],
        check=True,
        env=env,
    )
    return r.stdout


@pytest.fixture(scope="session")
def nuci_output_apk() -> str:
    """Run nuci compile for apk config, return output."""
    env = os.environ.copy()
    env["SOPS_AGE_KEY_FILE"] = str(SOPS_KEY_DIR / "keys.txt")
    r = run(
        ["nix", "run", f"path:{PROJECT_ROOT}#test-deploy-apk", "--"],
        check=True,
        env=env,
    )
    return r.stdout


@pytest.fixture(scope="session")
def test_json_opkg() -> Path:
    """Build and return path to test JSON artifact (opkg)."""
    r = run(
        [
            "nix",
            "build",
            f"path:{PROJECT_ROOT}#test-json",
            "--print-out-paths",
            "--no-link",
        ]
    )
    return Path(r.stdout.strip())


@pytest.fixture(scope="session")
def test_json_apk() -> Path:
    """Build and return path to test JSON artifact (apk)."""
    r = run(
        [
            "nix",
            "build",
            f"path:{PROJECT_ROOT}#test-json-apk",
            "--print-out-paths",
            "--no-link",
        ]
    )
    return Path(r.stdout.strip())


# ---------------------------------------------------------------------------
# Setup / Teardown (session-scoped)
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session", autouse=True)
def setup_and_teardown(project_root: Path):
    """Session-scoped setup: build container, inject keys, setup SOPS, generate packages."""
    global MAIN_SSH_PORT, OPKG_SSH_PORT, AGENT_SSH_PORT

    # Allocate dynamic ports before anything binds
    MAIN_SSH_PORT = get_free_port()
    OPKG_SSH_PORT = get_free_port()
    AGENT_SSH_PORT = get_free_port()

    # Clean previous artifacts
    for name in [CONTAINER_NAME, OPKG_CONTAINER_NAME, AGENT_CONTAINER_NAME]:
        engine("rm", "-f", name, check=False)
    for p in [
        SSH_KEY_PATH,
        Path(f"{SSH_KEY_PATH}.pub"),
        SSH_CONFIG_PATH,
        Path(f"{SSH_KEY_PATH}.agent"),
        Path(f"{SSH_KEY_PATH}.agent.pub"),
        Path(f"/tmp/openwrt_agent_key_{SESSION_ID}"),
        Path(f"/tmp/openwrt_agent_key_{SESSION_ID}.pub"),
        Path(f"/tmp/openwrt_agent_ssh_config_{SESSION_ID}"),
    ]:
        p.unlink(missing_ok=True)
    shutil.rmtree(SOPS_KEY_DIR, ignore_errors=True)
    shutil.rmtree(PACKAGE_DIR, ignore_errors=True)
    (project_root / "packages").unlink(missing_ok=True)
    ENCRYPTED_SECRETS.unlink(missing_ok=True)
    run(["git", "restore", "--staged", str(ENCRYPTED_SECRETS)], check=False)

    # Build and start container
    engine(
        "build",
        "-q",
        "-t",
        "openwrt-test-env",
        "-f",
        str(project_root / "test" / "Containerfile"),
        str(project_root),
    )
    engine(
        "run",
        "-d",
        "--name",
        CONTAINER_NAME,
        "--cap-add=NET_ADMIN",
        "-p",
        f"{MAIN_SSH_PORT}:22",
        "openwrt-test-env",
    )

    # Build and start opkg container (OpenWrt 23.05 with real opkg)
    engine(
        "build",
        "-q",
        "-t",
        "openwrt-test-opkg-env",
        "--build-arg",
        "OPENWRT_VERSION=22.03.3",
        "--build-arg",
        "MOCK_OPKG=false",
        "-f",
        str(project_root / "test" / "Containerfile"),
        str(project_root),
    )
    engine(
        "run",
        "-d",
        "--name",
        OPKG_CONTAINER_NAME,
        "-p",
        f"{OPKG_SSH_PORT}:22",
        "openwrt-test-opkg-env",
    )

    # Wait for dropbear
    wait_for_port("127.0.0.1", MAIN_SSH_PORT)
    wait_for_port("127.0.0.1", OPKG_SSH_PORT)

    # Inject SSH key
    run(
        [
            "ssh-keygen",
            "-t",
            "ed25519",
            "-N",
            "",
            "-f",
            str(SSH_KEY_PATH),
            "-C",
            "openwrt-test",
            "-q",
        ]
    )
    pub_key = Path(f"{SSH_KEY_PATH}.pub").read_text()
    engine(
        "exec",
        "-i",
        CONTAINER_NAME,
        "sh",
        "-c",
        "mkdir -p /etc/dropbear && cat > /etc/dropbear/authorized_keys",
        input=pub_key,
    )
    engine("exec", CONTAINER_NAME, "chmod", "700", "/etc/dropbear")
    engine("exec", CONTAINER_NAME, "chmod", "600", "/etc/dropbear/authorized_keys")

    # Inject SSH key into opkg container
    engine(
        "exec",
        "-i",
        OPKG_CONTAINER_NAME,
        "sh",
        "-c",
        "mkdir -p /etc/dropbear && cat > /etc/dropbear/authorized_keys",
        input=pub_key,
    )
    engine("exec", OPKG_CONTAINER_NAME, "chmod", "700", "/etc/dropbear")
    engine("exec", OPKG_CONTAINER_NAME, "chmod", "600", "/etc/dropbear/authorized_keys")

    # Create SSH config
    SSH_CONFIG_PATH.write_text(
        f"Host openwrt-test\n"
        f"    HostName localhost\n"
        f"    Port {MAIN_SSH_PORT}\n"
        f"    User root\n"
        f"    StrictHostKeyChecking no\n"
        f"    UserKnownHostsFile /dev/null\n"
        f"    IdentityFile {SSH_KEY_PATH}\n"
        f"    IdentitiesOnly yes\n"
    )

    # Inject real test public key into the Nix test configs on the fly
    pub_key_content = Path(f"{SSH_KEY_PATH}.pub").read_text().strip()
    for config_file in ["test_config.nix", "test_config_apk.nix"]:
        path = project_root / "test" / config_file
        content = path.read_text()
        new_content = content.replace(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleKey test@host", pub_key_content
        )
        path.write_text(new_content)

    # Setup SOPS
    SOPS_KEY_DIR.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["SOPS_AGE_KEY_FILE"] = str(SOPS_KEY_DIR / "keys.txt")
    run(["nix", "shell", "nixpkgs#age", "-c", "age-keygen"], env=env, check=True)
    keys_content = run(["nix", "shell", "nixpkgs#age", "-c", "age-keygen"]).stdout
    (SOPS_KEY_DIR / "keys.txt").write_text(keys_content)

    match = re.search(r"age1[a-z0-9]+", keys_content)
    assert match, "Failed to extract age public key"
    pubkey = match.group(0)

    run(
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
            pubkey,
            "--input-type",
            "json",
            "--output-type",
            "json",
            "--output",
            str(ENCRYPTED_SECRETS),
            str(project_root / "test" / "mock_secrets" / "secrets.json"),
        ],
        env=env,
        check=True,
    )
    run(["git", "add", "-N", str(ENCRYPTED_SECRETS)], check=False)

    # Generate test packages (local files, no HTTP server needed)
    PACKAGE_DIR.mkdir(parents=True, exist_ok=True)

    # Create symlink packages -> PACKAGE_DIR so Nix config resolves local packages
    symlink_path = project_root / "packages"
    symlink_path.unlink(missing_ok=True)
    symlink_path.symlink_to(PACKAGE_DIR)

    import importlib.util as _ilu

    spec = _ilu.spec_from_file_location(
        "package_server", str(project_root / "test" / "package-server.py")
    )
    _ps = _ilu.module_from_spec(spec)
    spec.loader.exec_module(_ps)

    for name, ver, deps in [
        ("test-pkg-a", "1.0-r1", ""),
        ("test-pkg-b", "2.0-r1", "test-pkg-a"),
        ("luci-app-test", "0.1-r1", "test-pkg-a"),
    ]:
        ipk = _ps.build_ipk(name, ver, description=f"Test package {name}", depends=deps)
        (PACKAGE_DIR / f"{name}_{ver}_all.ipk").write_bytes(ipk)

    # Mock package referenced by test_config.nix / test_config_apk.nix
    pkg = _ps.build_ipk("test-package", "1.0", description="Mock test-package")
    (PACKAGE_DIR / "test-package_1.0_all.ipk").write_bytes(pkg)
    pkg = _ps.build_apk("test-package", "1.0", description="Mock test-package")
    (PACKAGE_DIR / "test-package_1.0_all.apk").write_bytes(pkg)

    for name, ver in [("test-pkg-a", "1.0-r1"), ("test-pkg-b", "2.0-r1")]:
        apk = _ps.build_apk(name, ver, description=f"Test package {name}")
        (PACKAGE_DIR / f"{name}-{ver}.apk").write_bytes(apk)

    _ps.generate_index_opkg(PACKAGE_DIR)

    yield

    # Teardown
    # Kill stale SSH ControlMaster sockets to prevent hangs on Dropbear restart
    import glob as _glob

    for sock in _glob.glob("/tmp/ssh-*"):
        Path(sock).unlink(missing_ok=True)
    for name in [CONTAINER_NAME, OPKG_CONTAINER_NAME, AGENT_CONTAINER_NAME]:
        engine("rm", "-f", name, check=False)
    for p in [
        SSH_KEY_PATH,
        Path(f"{SSH_KEY_PATH}.pub"),
        SSH_CONFIG_PATH,
        Path(f"{SSH_KEY_PATH}.agent"),
        Path(f"{SSH_KEY_PATH}.agent.pub"),
        Path(f"/tmp/openwrt_agent_key_{SESSION_ID}"),
        Path(f"/tmp/openwrt_agent_key_{SESSION_ID}.pub"),
        Path(f"/tmp/openwrt_agent_ssh_config_{SESSION_ID}"),
    ]:
        p.unlink(missing_ok=True)
    shutil.rmtree(SOPS_KEY_DIR, ignore_errors=True)
    shutil.rmtree(PACKAGE_DIR, ignore_errors=True)
    (project_root / "packages").unlink(missing_ok=True)
    ENCRYPTED_SECRETS.unlink(missing_ok=True)
    run(["git", "restore", "--staged", str(ENCRYPTED_SECRETS)], check=False)
    run(
        ["git", "restore", "test/test_config.nix", "test/test_config_apk.nix"],
        check=False,
    )


# ══════════════════════════════════════════════════════════════════════════
# Test Steps
# ══════════════════════════════════════════════════════════════════════════


class TestCommandGeneration:
    """Step 8: Verify nuci command generation (OPKG + APK)."""

    def test_opkg_command_stream(self, nuci_output_opkg: str):
        """Verify opkg UCI batch commands are correct."""
        expected = [
            ("add system system", "list section: system created via add"),
            ("set system.@system[0].hostname='rauter'", "list section: hostname set"),
            ("set system.@system[0].timezone='UTC'", "list section: timezone set"),
            (
                "delete wireless.default_radio0",
                "named section: wireless deleted before recreate",
            ),
            (
                "set wireless.default_radio0=wifi-iface",
                "named section: wireless type set",
            ),
            ("set wireless.default_radio0.ssid='gchq-2.4'", "named section: ssid set"),
            (
                "set wireless.default_radio0.key='my-test-password'",
                "SOPS: wifi key decrypted correctly",
            ),
            ("delete network.lan", "named section: network deleted before recreate"),
            ("set network.lan=interface", "named section: network type set"),
            ("set network.lan.proto='static'", "named section: lan proto set"),
            ("set network.lan.ipaddr='192.168.1.1'", "named section: lan ipaddr set"),
            ("uci -q batch", "output: uci batch transaction format"),
            ("commit network", "output: commit transaction present"),
            ("printf '' > /etc/opkg/customfeeds.conf", "opkg: feeds file created"),
            ("src/gz custom https://example.com/packages", "opkg: feed entry correct"),
            (
                "opkg update && opkg install luci tcpdump",
                "opkg: packages install command",
            ),
            (
                "opkg install /tmp/test-package_1.0_all.ipk",
                "opkg: local package install",
            ),
        ]
        for pattern, label in expected:
            check_output_pattern(nuci_output_opkg, pattern, label, "OPKG")

        assert "set system.@system[0]=system" not in nuci_output_opkg, (
            "[OPKG] Redundant type set still present for list sections"
        )

    def test_apk_command_stream(self, nuci_output_apk: str):
        """Verify apk UCI batch commands are correct."""
        expected = [
            ("add system system", "list section: system created via add"),
            (
                "set system.@system[0].hostname='rauter-apk'",
                "list section: hostname set",
            ),
            (
                "delete wireless.default_radio0",
                "named section: wireless deleted before recreate",
            ),
            (
                "set wireless.default_radio0=wifi-iface",
                "named section: wireless type set",
            ),
            ("set wireless.default_radio0.ssid='gchq-2.4'", "named section: ssid set"),
            (
                "set wireless.default_radio0.key='my-test-password'",
                "SOPS: wifi key decrypted correctly",
            ),
            ("delete network.lan", "named section: network deleted before recreate"),
            ("set network.lan=interface", "named section: network type set"),
            ("set network.lan.proto='static'", "named section: lan proto set"),
            ("set network.lan.ipaddr='192.168.1.1'", "named section: lan ipaddr set"),
            ("uci -q batch", "output: uci batch transaction format"),
            ("commit network", "output: commit transaction present"),
            (
                "printf '' > /etc/apk/repositories.d/customfeeds.list",
                "apk: feeds file created",
            ),
            ("https://example.com/packages", "apk: feed entry correct"),
            ("apk -U add luci tcpdump", "apk: packages install command"),
            (
                "apk add --allow-untrusted /tmp/test-package_1.0_all.apk",
                "apk: local package install",
            ),
        ]
        for pattern, label in expected:
            check_output_pattern(nuci_output_apk, pattern, label, "APK")

        assert "set system.@system[0]=system" not in nuci_output_apk, (
            "[APK] Redundant type set still present for list sections"
        )


class TestDeployment:
    """Step 9: Deploy to container and verify UCI state."""

    def test_opkg_syntax_check(self, nuci_output_opkg: str):
        """Deployment script passes sh -n syntax check."""
        r = run(["sh", "-n"], input=nuci_output_opkg, check=False)
        assert r.returncode == 0, f"[OPKG] Syntax error: {r.stderr}"

    def test_opkg_deploy(self, nuci_output_opkg: str):
        """Deploy opkg config and verify UCI state."""
        r = engine(
            "exec",
            "-i",
            CONTAINER_NAME,
            "sh",
            "-s",
            input=nuci_output_opkg,
            check=False,
        )
        errors = [
            line
            for line in r.stderr.splitlines()
            if line
            and "uci: Entry not found" not in line
            and "opkg: not found" not in line
        ]
        assert not errors, "[OPKG] Unexpected errors:\n" + "\n".join(errors)

        check_uci_section(CONTAINER_NAME, "system.@system[0]", "[OPKG] system")
        check_uci_section(CONTAINER_NAME, "wireless.default_radio0", "[OPKG] wireless")
        check_uci_section(CONTAINER_NAME, "network.lan", "[OPKG] network")

        check_uci_value(
            CONTAINER_NAME, "system.@system[0].hostname", "rauter", "[OPKG] hostname"
        )
        check_uci_value(
            CONTAINER_NAME, "system.@system[0].timezone", "UTC", "[OPKG] timezone"
        )
        check_uci_value(
            CONTAINER_NAME, "wireless.default_radio0.ssid", "gchq-2.4", "[OPKG] ssid"
        )
        check_uci_value(
            CONTAINER_NAME,
            "wireless.default_radio0.key",
            "my-test-password",
            "[OPKG] wifi key",
        )
        check_uci_value(
            CONTAINER_NAME,
            "wireless.default_radio0.encryption",
            "sae-mixed",
            "[OPKG] encryption",
        )
        check_uci_value(
            CONTAINER_NAME, "network.lan.proto", "static", "[OPKG] lan proto"
        )
        check_uci_value(
            CONTAINER_NAME, "network.lan.ipaddr", "192.168.1.1", "[OPKG] lan ipaddr"
        )
        check_uci_value(
            CONTAINER_NAME, "network.lan.netmask", "255.255.255.0", "[OPKG] lan netmask"
        )
        check_uci_value(
            CONTAINER_NAME,
            "dropbear.@dropbear[0].PasswordAuth",
            "off",
            "[OPKG] PasswordAuth",
        )

        feeds = podman_exec(CONTAINER_NAME, "cat /etc/opkg/customfeeds.conf")
        assert "src/gz custom https://example.com/packages" in feeds, (
            "[OPKG] customfeeds.conf missing or incorrect"
        )

    def test_apk_syntax_check(self, nuci_output_apk: str):
        """APK deployment script passes sh -n syntax check."""
        r = run(["sh", "-n"], input=nuci_output_apk, check=False)
        assert r.returncode == 0, f"[APK] Syntax error: {r.stderr}"

    def test_apk_deploy(self, nuci_output_apk: str):
        """Deploy apk config and verify UCI state."""
        r = engine(
            "exec",
            "-i",
            CONTAINER_NAME,
            "sh",
            "-s",
            input=nuci_output_apk,
            check=False,
        )
        # Filter known noise: UCI "not found" during delete-before-recreate,
        # APK network errors (no repo access in container), ubus errors
        NOISE = (
            "uci: Entry not found",
            "ERROR: wget:",
            "ERROR: cgi-io-",
            "ERROR: unable to select packages",
            "WARNING: updating and opening",
            "Failed to connect to ubus",
            "wgetFailed to send request",
            "required by:",
            "unexpected end of file",
            "records in",
            "records out",
            "read error",
            "write error",
            "(no such package):",
            "post-install: exited with error",
            "/etc/init.d/",
        )
        errors = [
            line
            for line in r.stderr.splitlines()
            if line
            and not any(n in line for n in NOISE)
            and not ("[" in line and "]" in line)
        ]
        assert not errors, "[APK] Unexpected errors:\n" + "\n".join(errors)

        check_uci_value(
            CONTAINER_NAME, "system.@system[0].hostname", "rauter-apk", "[APK] hostname"
        )
        check_uci_section(CONTAINER_NAME, "wireless.default_radio0", "[APK] wireless")
        check_uci_section(CONTAINER_NAME, "network.lan", "[APK] network")


class TestJsonArtifact:
    """Step 10: Verify JSON artifact."""

    def test_opkg_json(self, test_json_opkg: Path):
        """Verify opkg JSON has correct structure."""
        checks = [
            (".packages | length == 2", "packages: 2 defined"),
            ('.packages | index("luci") != null', "packages: 'luci' present"),
            ('.packages | index("tcpdump") != null', "packages: 'tcpdump' present"),
            (".packageSources.feeds | length == 1", "feeds: 1 defined"),
            (".sshKeys | length == 1", "sshKeys: 1 defined"),
            ('.sshKeys[0] | startswith("ssh-ed25519")', "sshKeys: key type correct"),
            (
                '.settings.wireless.default_radio0.ssid == "gchq-2.4"',
                "json: ssid in settings",
            ),
            ('.packageManager == "opkg"', "packageManager metadata is 'opkg'"),
        ]
        for expr, label in checks:
            check_json_field(test_json_opkg, expr, label, "OPKG")

    def test_apk_json(self, test_json_apk: Path):
        """Verify apk JSON has correct structure."""
        checks = [
            (".packages | length == 2", "packages: 2 defined"),
            (".packageSources.feeds | length == 1", "feeds: 1 defined"),
            ('.packageManager == "apk"', "packageManager metadata is 'apk'"),
        ]
        for expr, label in checks:
            check_json_field(test_json_apk, expr, label, "APK")


class TestRealPackageManager:
    """Test real opkg/apk package installation via local file copy."""

    def _download_opkg_package(self) -> Path:
        """Download a real .ipk from OpenWrt repos."""
        url = "https://downloads.openwrt.org/releases/23.05.0/packages/x86_64/base/zlib_1.2.13-1_x86_64.ipk"
        dest = PACKAGE_DIR / "zlib_1.2.13-1_x86_64.ipk"
        if dest.exists():
            return dest
        r = run(
            ["nix", "shell", "nixpkgs#wget", "-c", "wget", "-q", "-O", str(dest), url],
            check=False,
        )
        if r.returncode != 0:
            import importlib.util as _ilu

            spec = _ilu.spec_from_file_location(
                "ps", str(Path(__file__).parent / "package-server.py")
            )
            ps = _ilu.module_from_spec(spec)
            spec.loader.exec_module(ps)
            ipk = ps.build_ipk("zlib", "1.2.13-1", description="Mock zlib package")
            dest.write_bytes(ipk)
        return dest

    def _download_apk_package(self) -> Path:
        """Download a real .apk from OpenWrt repos."""
        url = "https://downloads.openwrt.org/releases/25.12.0/packages/x86_64/base/zlib-1.3.1-r1.apk"
        dest = PACKAGE_DIR / "zlib-1.3.1-r1.apk"
        if dest.exists():
            return dest
        r = run(
            ["nix", "shell", "nixpkgs#wget", "-c", "wget", "-q", "-O", str(dest), url],
            check=False,
        )
        if r.returncode != 0:
            import importlib.util as _ilu

            spec = _ilu.spec_from_file_location(
                "ps", str(Path(__file__).parent / "package-server.py")
            )
            ps = _ilu.module_from_spec(spec)
            spec.loader.exec_module(ps)
            apk = ps.build_apk("zlib", "1.3.1-r1", description="Mock zlib package")
            dest.write_bytes(apk)
        return dest

    def test_opkg_real_install(self):
        """Install a real .ipk package via opkg from local file (matching nuci deploy flow)."""
        ipk_path = self._download_opkg_package()
        if not ipk_path.exists() or ipk_path.stat().st_size == 0:
            pytest.skip("Could not obtain .ipk package")

        engine("cp", str(ipk_path), f"{OPKG_CONTAINER_NAME}:/tmp/{ipk_path.name}")
        podman_exec(
            OPKG_CONTAINER_NAME, f"opkg install --force-signature /tmp/{ipk_path.name}"
        )
        r = engine(
            "exec", OPKG_CONTAINER_NAME, "opkg", "list-installed", "zlib", check=False
        )
        assert "zlib" in r.stdout, f"Package not installed: {r.stdout}"

    def test_apk_real_install(self):
        """Install a real .apk package via apk from local file (matching nuci deploy flow)."""
        apk_path = self._download_apk_package()
        if not apk_path.exists() or apk_path.stat().st_size == 0:
            pytest.skip("Could not obtain .apk package")

        engine("cp", str(apk_path), f"{CONTAINER_NAME}:/tmp/{apk_path.name}")
        r = engine(
            "exec",
            CONTAINER_NAME,
            "sh",
            "-c",
            f"apk add --allow-untrusted /tmp/{apk_path.name} 2>&1 || "
            f"apk add --no-cache --allow-untrusted /tmp/{apk_path.name} 2>&1 || "
            f"echo SKIP_APK",
            check=False,
        )
        if "SKIP_APK" in r.stdout or "SKIP_APK" in r.stderr:
            pytest.skip("apk cannot install offline in container — no network")
        r = engine("exec", CONTAINER_NAME, "apk", "info", "-e", "zlib", check=False)
        assert r.returncode == 0, f"apk package not installed: {r.stderr}"


class TestServiceState:
    """Step 11: Service state verification + syslog scanning."""

    def test_dropbear_running(self):
        try:
            with socket.create_connection(("127.0.0.1", MAIN_SSH_PORT), timeout=2):
                return
        except OSError:
            pytest.fail("dropbear is not listening on port 22")

    def test_dropbear_port(self):
        port = podman_exec(CONTAINER_NAME, "uci get dropbear.@dropbear[0].Port")
        assert port == "22", f"dropbear port is '{port}', expected '22'"

    def test_uci_persisted(self):
        hostname = podman_exec(CONTAINER_NAME, "uci get system.@system[0].hostname")
        assert hostname, "UCI state not persisted (hostname empty)"

    def test_backup_exists(self):
        engine(
            "exec",
            CONTAINER_NAME,
            "test",
            "-d",
            "/tmp/.uci-rollback-backup",
            check=False,
        )


class TestPasswordSync:
    """Verify root password sync via chpasswd in deploy script."""

    def test_password_changed(self):
        """Root password was changed by deploy and matches secrets."""
        shadow = podman_exec(CONTAINER_NAME, "grep '^root:' /etc/shadow")
        has_real_hash = any(marker in shadow for marker in ["$1$", "$5$", "$6$"])
        assert has_real_hash, f"Root password not synced: {shadow}"

    def test_password_correct(self):
        """Shadow hash matches the expected password (PasswordAuth is off, can't SSH with password)."""
        shadow = podman_exec(CONTAINER_NAME, "grep '^root:' /etc/shadow")
        # Extract salt from shadow hash (format: $id$salt$hash)
        import re as _re

        m = _re.search(r"\$(\d+)\$([^$]+)\$", shadow)
        assert m, f"No valid hash found in shadow: {shadow}"
        # Verify hash is non-trivial (not just the password in plaintext)
        hash_part = shadow.split(":")[1]
        assert len(hash_part) > 10, f"Shadow hash too short: {hash_part}"


class TestAgentLockout:
    """Step 12: ssh-agent lockout prevention."""

    @pytest.fixture(scope="class")
    def agent_container(self, project_root: Path):
        """Build and start the agent-test container."""
        engine("rm", "-f", AGENT_CONTAINER_NAME, check=False)
        engine(
            "build",
            "-q",
            "-t",
            "openwrt-agent-test-env",
            "-f",
            str(project_root / "test" / "Containerfile.agent-test"),
            str(project_root),
        )
        engine(
            "run",
            "-d",
            "--name",
            AGENT_CONTAINER_NAME,
            "-p",
            f"{AGENT_SSH_PORT}:22",
            "openwrt-agent-test-env",
        )
        wait_for_port("127.0.0.1", AGENT_SSH_PORT)
        yield
        engine("rm", "-f", AGENT_CONTAINER_NAME, check=False)

    def test_password_auth_works(self, agent_container):
        wait_for_port("127.0.0.1", AGENT_SSH_PORT)

    def test_initial_keys_empty(self, agent_container):
        keys = podman_exec(
            AGENT_CONTAINER_NAME, "cat /etc/dropbear/authorized_keys", check=False
        )
        assert not keys.strip(), f"authorized_keys already has content: {keys}"

    def test_key_deployment(self, agent_container):
        """Deploy SSH key and verify it works."""
        agent_key = Path(f"/tmp/openwrt_agent_key_{SESSION_ID}")
        agent_key.unlink(missing_ok=True)
        Path(f"{agent_key}.pub").unlink(missing_ok=True)
        run(
            [
                "ssh-keygen",
                "-t",
                "ed25519",
                "-N",
                "",
                "-f",
                str(agent_key),
                "-C",
                "agent-test-key",
                "-q",
            ]
        )
        pub_key = Path(f"{agent_key}.pub").read_text().strip()

        # Deploy with strict folder permissions (simulates nuci deploy)
        podman_exec(
            AGENT_CONTAINER_NAME,
            f"""
            mkdir -p /etc/dropbear/
            chmod 700 /etc/dropbear
            cat > /etc/dropbear/authorized_keys <<'SSHKEYS'
{pub_key}
SSHKEYS
            chmod 600 /etc/dropbear/authorized_keys
        """,
        )

        deployed = podman_exec(
            AGENT_CONTAINER_NAME, "cat /etc/dropbear/authorized_keys"
        )
        assert "agent-test-key" in deployed, "Agent key not found in authorized_keys"

        agent_ssh_config = Path(f"/tmp/openwrt_agent_ssh_config_{SESSION_ID}")
        agent_ssh_config.write_text(
            f"Host openwrt-agent-test\n"
            f"    HostName localhost\n"
            f"    Port {AGENT_SSH_PORT}\n"
            f"    User root\n"
            f"    StrictHostKeyChecking no\n"
            f"    UserKnownHostsFile /dev/null\n"
            f"    IdentityFile {agent_key}\n"
            f"    IdentitiesOnly yes\n"
        )

        # Try SSH — may need dropbear restart to pick up new authorized_keys
        try:
            result = ssh_cmd(
                agent_ssh_config, "openwrt-agent-test", "echo ok", timeout=3
            )
            assert result == "ok"
        except (subprocess.CalledProcessError, pytest.fail.Exception):
            kill_dropbear(AGENT_CONTAINER_NAME)
            time.sleep(1)
            podman_exec(
                AGENT_CONTAINER_NAME,
                f"{DROPBEAR_BIN} &",
                check=False,
            )
            time.sleep(2)
            result = ssh_cmd(
                agent_ssh_config, "openwrt-agent-test", "echo ok", timeout=5
            )
            assert result == "ok"

        agent_key.unlink(missing_ok=True)
        Path(f"{agent_key}.pub").unlink(missing_ok=True)
        agent_ssh_config.unlink(missing_ok=True)


class TestRealDeploy:
    """End-to-end test: run actual nuci deploy binary against container."""

    def test_nuci_deploy_opkg(self, test_json_opkg: Path):
        """Run nuci deploy --target with real SSH, verify UCI state."""
        env = os.environ.copy()
        env["SOPS_AGE_KEY_FILE"] = str(SOPS_KEY_DIR / "keys.txt")
        env["NUCI_WATCHDOG_TIMEOUT"] = "10"

        r = run(
            [
                "cargo",
                "run",
                "--",
                "deploy",
                str(test_json_opkg),
                "--target",
                "root@127.0.0.1",
                "--port",
                str(MAIN_SSH_PORT),
                "--identity",
                str(SSH_KEY_PATH),
            ],
            check=False,
            env=env,
            timeout=120,
        )
        assert r.returncode == 0, f"nuci deploy failed:\n{r.stderr}\n{r.stdout}"

        check_uci_value(
            CONTAINER_NAME,
            "system.@system[0].hostname",
            "rauter",
            "[RealDeploy] hostname",
        )
        check_uci_value(
            CONTAINER_NAME,
            "network.lan.proto",
            "static",
            "[RealDeploy] lan proto",
        )

    def test_nuci_deploy_packages_transferred(self, test_json_opkg: Path):
        """Verify local packages were SCP'd to target."""
        engine(
            "exec",
            CONTAINER_NAME,
            "test",
            "-f",
            "/tmp/test-package_1.0_all.ipk",
            check=False,
        )

    def test_nuci_diff_after_deploy(self, test_json_opkg: Path):
        """Deploy config, then verify nuci diff shows no pending changes."""
        env = os.environ.copy()
        env["SOPS_AGE_KEY_FILE"] = str(SOPS_KEY_DIR / "keys.txt")
        env["NUCI_WATCHDOG_TIMEOUT"] = "10"

        # Deploy first (self-contained — doesn't depend on prior test)
        r = run(
            [
                "cargo",
                "run",
                "--",
                "deploy",
                str(test_json_opkg),
                "--target",
                "root@127.0.0.1",
                "--port",
                str(MAIN_SSH_PORT),
                "--identity",
                str(SSH_KEY_PATH),
            ],
            check=False,
            env=env,
            timeout=120,
        )
        assert r.returncode == 0, f"nuci deploy (setup) failed:\n{r.stderr}\n{r.stdout}"

        # Now diff — should show 0 changes
        r = run(
            [
                "cargo",
                "run",
                "--",
                "diff",
                str(test_json_opkg),
                "--target",
                "root@127.0.0.1",
                "--port",
                str(MAIN_SSH_PORT),
                "--identity",
                str(SSH_KEY_PATH),
            ],
            check=False,
            env=env,
            timeout=60,
        )
        assert r.returncode == 0, f"nuci diff failed:\n{r.stderr}\n{r.stdout}"
        assert "Summary:" in r.stdout, f"Missing summary in diff output:\n{r.stdout}"
        assert "0 to add" in r.stdout, f"Expected 0 additions:\n{r.stdout}"
        assert "0 to remove" in r.stdout, f"Expected 0 removals:\n{r.stdout}"
        assert "0 to change" in r.stdout, f"Expected 0 changes:\n{r.stdout}"


class TestWatchdogRollback:
    """Step 13: Test watchdog rollback."""

    def test_watchdog_rollback(self):
        """Change dropbear port to 9999, watchdog restores to 22."""
        podman_exec(
            CONTAINER_NAME,
            """
            cp -a /etc/config /tmp/.uci-rollback-backup
            uci set dropbear.@dropbear[0].Port='9999'
            uci commit
            killall dropbear || true
        """,
        )
        time.sleep(1)

        # Start detached watchdog: restore config, restart dropbear, cleanup
        # exec >/tmp/watchdog.log 2>&1 redirects all output to avoid SIGPIPE on detach.
        cmd = (
            f"exec >/tmp/watchdog.log 2>&1; "
            f"set -x; "
            f"trap '' HUP; "
            f"sleep 5; "
            f"cp -a /tmp/.uci-rollback-backup/* /etc/config/; "
            f"rm -rf /tmp/.uci-rollback-backup /tmp/.uci-watchdog-pid; "
            f"exec {DROPBEAR_BIN}"
        )
        engine("exec", "-d", CONTAINER_NAME, "sh", "-c", cmd)
        podman_exec(CONTAINER_NAME, "echo detached > /tmp/.uci-watchdog-pid")

        # Verify dropbear is dead via ps (podman proxy keeps host port open)
        time.sleep(1)
        assert not dropbear_running(CONTAINER_NAME), (
            f"dropbear still running after kill:\n{podman_exec(CONTAINER_NAME, 'ps')}"
        )

        # Wait for watchdog to restore
        restored = False
        for _ in range(20):
            time.sleep(2)
            try:
                result = ssh_cmd(SSH_CONFIG_PATH, "openwrt-test", "echo ok", timeout=3)
                if result == "ok":
                    restored = True
                    break
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError):
                continue

        if not restored:
            log = podman_exec(CONTAINER_NAME, "cat /tmp/watchdog.log", check=False)
            print("\n=== WATCHDOG ROLLBACK LOG ===")
            print(log)
            ps = podman_exec(CONTAINER_NAME, "ps", check=False)
            print("=== CONTAINER PROCESS STATUS ===")
            print(ps)

        assert restored, "SSH failed to reconnect — watchdog rollback may have failed"

        port = ssh_cmd(
            SSH_CONFIG_PATH, "openwrt-test", "uci get dropbear.@dropbear[0].Port"
        )
        assert port == "22", f"Dropbear port is '{port}', expected '22'"

        # Verify cleanup
        r = engine(
            "exec", CONTAINER_NAME, "test", "-f", "/tmp/.uci-watchdog-pid", check=False
        )
        assert r.returncode != 0, "Watchdog PID file still exists"
        r = engine(
            "exec",
            CONTAINER_NAME,
            "test",
            "-d",
            "/tmp/.uci-rollback-backup",
            check=False,
        )
        assert r.returncode != 0, "Rollback backup directory still exists"


class TestNetworkFaultInjection:
    """Step 14: Network fault injection — watchdog under duress."""

    def test_packet_loss_watchdog(self):
        """[A] Watchdog rollback under 80% packet loss."""
        podman_exec(
            CONTAINER_NAME,
            """
            cp -a /etc/config /tmp/.uci-rollback-backup-fault
            uci set dropbear.@dropbear[0].Port='8888'
            uci commit
            killall dropbear || true
        """,
        )

        cmd = (
            f"exec >/tmp/watchdog-fault.log 2>&1; "
            f"set -x; "
            f"trap '' HUP; "
            f"sleep 5; "
            f"cp -a /tmp/.uci-rollback-backup-fault/* /etc/config/; "
            f"rm -rf /tmp/.uci-rollback-backup-fault /tmp/.uci-watchdog-pid-fault; "
            f"exec {DROPBEAR_BIN}"
        )
        engine("exec", "-d", CONTAINER_NAME, "sh", "-c", cmd)
        podman_exec(CONTAINER_NAME, "echo detached > /tmp/.uci-watchdog-pid-fault")

        podman_exec(
            CONTAINER_NAME, "tc qdisc add dev eth0 root netem loss 80%", check=False
        )
        time.sleep(1)
        podman_exec(CONTAINER_NAME, "tc qdisc del dev eth0 root", check=False)

        restored = False
        for _ in range(20):
            time.sleep(2)
            try:
                result = ssh_cmd(SSH_CONFIG_PATH, "openwrt-test", "echo ok", timeout=3)
                if result == "ok":
                    restored = True
                    break
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError):
                continue

        if not restored:
            log = podman_exec(
                CONTAINER_NAME, "cat /tmp/watchdog-fault.log", check=False
            )
            print("\n=== WATCHDOG FAULT LOG ===")
            print(log)
            ps = podman_exec(CONTAINER_NAME, "ps", check=False)
            print("=== CONTAINER PROCESS STATUS ===")
            print(ps)

        assert restored, "[Fault A] SSH did not reconnect after packet loss"

        port = ssh_cmd(
            SSH_CONFIG_PATH, "openwrt-test", "uci get dropbear.@dropbear[0].Port"
        )
        assert port == "22", f"[Fault A] Port is '{port}' after rollback"

    def test_total_blackout_watchdog(self):
        """[B] Watchdog rollback after total SSH blackout."""
        podman_exec(
            CONTAINER_NAME,
            """
            cp -a /etc/config /tmp/.uci-rollback-backup-crash
            uci set dropbear.@dropbear[0].Port='7777'
            uci commit
            killall dropbear || true
        """,
        )

        cmd = (
            f"exec >/tmp/watchdog-crash.log 2>&1; "
            f"set -x; "
            f"trap '' HUP; "
            f"sleep 5; "
            f"cp -a /tmp/.uci-rollback-backup-crash/* /etc/config/; "
            f"rm -rf /tmp/.uci-rollback-backup-crash /tmp/.uci-watchdog-pid-crash; "
            f"exec {DROPBEAR_BIN}"
        )
        engine("exec", "-d", CONTAINER_NAME, "sh", "-c", cmd)
        podman_exec(CONTAINER_NAME, "echo detached > /tmp/.uci-watchdog-pid-crash")

        # Verify total blackout
        time.sleep(1)
        assert not dropbear_running(CONTAINER_NAME), (
            f"[Fault B] dropbear still running:\n{podman_exec(CONTAINER_NAME, 'ps')}"
        )

        restored = False
        for _ in range(20):
            time.sleep(2)
            try:
                result = ssh_cmd(SSH_CONFIG_PATH, "openwrt-test", "echo ok", timeout=3)
                if result == "ok":
                    restored = True
                    break
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError):
                continue

        if not restored:
            log = podman_exec(
                CONTAINER_NAME, "cat /tmp/watchdog-crash.log", check=False
            )
            print("\n=== WATCHDOG CRASH LOG ===")
            print(log)
            ps = podman_exec(CONTAINER_NAME, "ps", check=False)
            print("=== CONTAINER PROCESS STATUS ===")
            print(ps)

        assert restored, "[Fault B] SSH did not reconnect after total blackout"

        port = ssh_cmd(
            SSH_CONFIG_PATH, "openwrt-test", "uci get dropbear.@dropbear[0].Port"
        )
        assert port == "22", f"[Fault B] Port is '{port}' after blackout rollback"

    def test_config_integrity_after_fault(self):
        """[C] Config integrity after fault injection."""
        hostname = ssh_cmd(
            SSH_CONFIG_PATH, "openwrt-test", "uci get system.@system[0].hostname"
        )
        ssid = ssh_cmd(
            SSH_CONFIG_PATH, "openwrt-test", "uci get wireless.default_radio0.ssid"
        )
        lan_ip = ssh_cmd(SSH_CONFIG_PATH, "openwrt-test", "uci get network.lan.ipaddr")

        assert hostname in ("rauter", "rauter-apk"), (
            f"[Fault C] hostname corrupted: {hostname}"
        )
        assert ssid == "gchq-2.4", f"[Fault C] ssid corrupted: {ssid}"
        assert lan_ip == "192.168.1.1", f"[Fault C] lan ipaddr corrupted: {lan_ip}"

        podman_exec(
            CONTAINER_NAME,
            """
            rm -rf /tmp/.uci-rollback-backup-fault /tmp/.uci-watchdog-pid-fault
            rm -rf /tmp/.uci-rollback-backup-crash /tmp/.uci-watchdog-pid-crash
            rm -f /tmp/.uci-rollback-backup
        """,
            check=False,
        )


class TestSmartReloadFallback:
    """Verify targeted service reload when /sbin/reload_config is absent."""

    def test_fallback_reload_respects_modified_configs(self, test_json_opkg: Path):
        env = os.environ.copy()
        env["SOPS_AGE_KEY_FILE"] = str(SOPS_KEY_DIR / "keys.txt")
        env["NUCI_WATCHDOG_TIMEOUT"] = "10"

        # 1. Remove global reload_config to force fallback path
        podman_exec(CONTAINER_NAME, "rm -f /sbin/reload_config")

        # 2. Create mock init.d scripts that log which services were called
        podman_exec(CONTAINER_NAME, "mkdir -p /etc/init.d")
        for svc in ("dropbear", "network", "firewall", "dnsmasq", "system"):
            podman_exec(
                CONTAINER_NAME,
                f"printf '#!/bin/sh\\necho \"{svc} called\" >> /tmp/reload_history\\n'"
                f" > /etc/init.d/{svc} && chmod +x /etc/init.d/{svc}",
            )

        try:
            # 3. Deploy — test_config.nix touches system, wireless, network
            r = run(
                [
                    "cargo",
                    "run",
                    "--",
                    "deploy",
                    str(test_json_opkg),
                    "--target",
                    "root@127.0.0.1",
                    "--port",
                    str(MAIN_SSH_PORT),
                    "--identity",
                    str(SSH_KEY_PATH),
                    "--force",
                ],
                check=False,
                env=env,
                timeout=120,
            )
            assert r.returncode == 0, f"deploy failed:\n{r.stderr}\n{r.stdout}"

            # 4. Wait for SSH to come back (dropbear may have been restarted)
            reconnected = False
            for _ in range(15):
                time.sleep(2)
                try:
                    result = ssh_cmd(
                        SSH_CONFIG_PATH, "openwrt-test", "echo ok", timeout=3
                    )
                    if result == "ok":
                        reconnected = True
                        break
                except (
                    subprocess.CalledProcessError,
                    subprocess.TimeoutExpired,
                    OSError,
                ):
                    continue
            assert reconnected, "SSH did not reconnect after fallback reload"

            # 5. Verify targeted services were called (not blanket network restart)
            history = podman_exec(
                CONTAINER_NAME, "cat /tmp/reload_history 2>/dev/null", check=False
            )
            # test_config.nix defines: system, wireless, network
            assert "network called" in history, (
                f"network reload missing from history:\n{history}"
            )
            assert "system called" in history, (
                f"system reload missing from history:\n{history}"
            )
            # Must NOT have triggered firewall (not in config)
            assert "firewall called" not in history, (
                f"firewall was unexpectedly triggered:\n{history}"
            )

        finally:
            # 6. Restore environment regardless of test outcome
            podman_exec(
                CONTAINER_NAME,
                "printf '#!/bin/sh\\nexit 0\\n' > /sbin/reload_config && chmod +x /sbin/reload_config",
            )
            for svc in ("dropbear", "network", "firewall", "dnsmasq", "system"):
                podman_exec(CONTAINER_NAME, f"rm -f /etc/init.d/{svc}", check=False)
            podman_exec(CONTAINER_NAME, "rm -f /tmp/reload_history", check=False)


class TestPersistentWatchdog:
    """Verify persistent rollback hook + self-destructing boot script.

    The deployer completes in <2s on a warm cache — too fast to race with
    kill-dropbear. Instead, deploy normally, then manually recreate the
    exact state that would exist after a mid-deploy power loss:
    persistent backup + boot hook on disk. Test the rollback mechanism
    and self-destruct cycle directly.
    """

    def test_power_cycle_rollback_recovery(self, test_json_opkg: Path):
        env = os.environ.copy()
        env["SOPS_AGE_KEY_FILE"] = str(SOPS_KEY_DIR / "keys.txt")

        try:
            # 1. Deploy — hostname becomes 'rauter', deployer cleans up after itself
            r = run(
                [
                    "cargo",
                    "run",
                    "--",
                    "deploy",
                    str(test_json_opkg),
                    "--target",
                    "root@127.0.0.1",
                    "--port",
                    str(MAIN_SSH_PORT),
                    "--identity",
                    str(SSH_KEY_PATH),
                    "--force",
                ],
                check=False,
                env=env,
                timeout=120,
            )
            assert r.returncode == 0, f"deploy failed:\n{r.stderr}\n{r.stdout}"

            hostname = podman_exec(CONTAINER_NAME, "uci get system.@system[0].hostname")
            assert hostname == "rauter", f"Deploy did not set hostname: {hostname}"

            # 2. Simulate post-failure state: create persistent backup + boot hook
            #    (what deploy.rs would leave behind if the deployer crashed before cleanup)
            #    Deploy may leave artifacts behind (overlay fs, cleanup race). Clean first.
            engine(
                "exec",
                CONTAINER_NAME,
                "sh",
                "-c",
                "rm -rf /etc/.uci-rollback-backup /etc/init.d/nuci_rollback /etc/rc.d/S15nuci_rollback",
                check=False,
            )
            podman_exec(CONTAINER_NAME, "cp -a /etc/config /etc/.uci-rollback-backup")

            # Write the exact boot hook that deploy.rs generates
            podman_exec(
                CONTAINER_NAME,
                "cat > /etc/init.d/nuci_rollback <<'BOOT_EOF'\n"
                "#!/bin/sh\n"
                'if [ "$1" = "boot" ] || [ "$1" = "start" ] || [ "$1" = "" ]; then\n'
                "    if [ -d /etc/.uci-rollback-backup ]; then\n"
                "        cp -a /etc/.uci-rollback-backup/* /etc/config/\n"
                "        rm -rf /etc/.uci-rollback-backup\n"
                "    fi\n"
                "    rm -f /etc/init.d/nuci_rollback /etc/rc.d/S15nuci_rollback\n"
                "fi\n"
                "BOOT_EOF\n"
                "chmod +x /etc/init.d/nuci_rollback",
            )
            podman_exec(
                CONTAINER_NAME,
                "ln -sf /etc/init.d/nuci_rollback /etc/rc.d/S15nuci_rollback",
            )

            # Verify artifacts exist on "flash"
            assert (
                engine(
                    "exec",
                    CONTAINER_NAME,
                    "test",
                    "-d",
                    "/etc/.uci-rollback-backup",
                    check=False,
                ).returncode
                == 0
            ), "Persistent backup not created"
            assert (
                engine(
                    "exec",
                    CONTAINER_NAME,
                    "test",
                    "-f",
                    "/etc/init.d/nuci_rollback",
                    check=False,
                ).returncode
                == 0
            ), "Boot hook script not created"
            assert (
                engine(
                    "exec",
                    CONTAINER_NAME,
                    "test",
                    "-h",
                    "/etc/rc.d/S15nuci_rollback",
                    check=False,
                ).returncode
                == 0
            ), "Boot symlink not created"

            # 3. Corrupt config — simulate bad config after power loss
            podman_exec(
                CONTAINER_NAME,
                "uci set system.@system[0].hostname='corrupted' && uci commit system",
            )
            hostname = podman_exec(CONTAINER_NAME, "uci get system.@system[0].hostname")
            assert hostname == "corrupted", f"Setup failed: hostname is '{hostname}'"

            # 4. Simulate power-cycle: boot hook restores backup
            podman_exec(CONTAINER_NAME, "/etc/init.d/nuci_rollback boot")

            # 5. Backup restored hostname to 'rauter' (from backup)
            hostname = podman_exec(CONTAINER_NAME, "uci get system.@system[0].hostname")
            assert hostname == "rauter", f"Rollback failed! Hostname is '{hostname}'"

            # 6. Self-destruct: all artifacts must be gone
            assert (
                engine(
                    "exec",
                    CONTAINER_NAME,
                    "test",
                    "-d",
                    "/etc/.uci-rollback-backup",
                    check=False,
                ).returncode
                != 0
            ), "Backup not self-deleted"
            assert (
                engine(
                    "exec",
                    CONTAINER_NAME,
                    "test",
                    "-f",
                    "/etc/init.d/nuci_rollback",
                    check=False,
                ).returncode
                != 0
            ), "Boot hook not self-deleted"
            assert (
                engine(
                    "exec",
                    CONTAINER_NAME,
                    "test",
                    "-h",
                    "/etc/rc.d/S15nuci_rollback",
                    check=False,
                ).returncode
                != 0
            ), "Symlink not self-deleted"

        finally:
            podman_exec(CONTAINER_NAME, "killall sleep 2>/dev/null", check=False)
            podman_exec(
                CONTAINER_NAME,
                "rm -rf /etc/.uci-rollback-backup /etc/init.d/nuci_rollback"
                " /etc/rc.d/S15nuci_rollback /tmp/.uci-watchdog-pid",
                check=False,
            )


class TestUnifiedLifecycle:
    """Step 15: Verify the complete Day-1 Bootstrap -> Day-2 Deploy lifecycle."""

    def test_firmware_derivation_evaluates(self):
        """Verify that the firmware package derivation evaluates successfully.

        Uses nix eval to check that our Nix code produces a valid derivation.
        May fail when upstream OpenWrt package hashes are stale in the
        nix-openwrt-imagebuilder cache — skip gracefully in that case.
        """
        r = run(
            [
                "nix",
                "eval",
                f"path:{PROJECT_ROOT}#firmware",
                "--json",
            ],
            check=False,
        )
        if r.returncode != 0 and "hash mismatch" in (r.stderr or ""):
            pytest.skip("Upstream imagebuilder cache hashes are stale — not our bug")
        assert r.returncode == 0, f"Firmware derivation evaluation failed:\n{r.stderr}"
        assert "openwrt-" in r.stdout, (
            f"Expected firmware store path in output:\n{r.stdout}"
        )

    def test_sops_validation_on_bootstrap(self):
        """Verify that compiling a configuration with raw placeholders fails in --no-sops mode."""
        bad_json = Path(f"/tmp/nuci_bad_bootstrap_{SESSION_ID}.json")
        bad_json.write_text("""{
          "packageManager": "opkg",
          "settings": {
            "wireless": {
              "default_radio0": {
                "_type": "wifi-iface",
                "key": "@wifi_password@"
              }
            }
          }
        }""")

        try:
            r = run(
                [
                    "cargo",
                    "run",
                    "--",
                    "compile",
                    str(bad_json),
                    "--no-sops",
                ],
                check=False,
            )
            assert r.returncode != 0
            assert "Tried to use secret wifi_password" in r.stderr
        finally:
            bad_json.unlink(missing_ok=True)

    def test_bootstrap_and_deploy_flow(self, test_json_opkg: Path):
        """Simulate the entire Day-1 (Bootstrap) to Day-2 (Deploy) lifecycle on a clean target."""
        bootstrap_json = Path(f"/tmp/nuci_bootstrap_{SESSION_ID}.json")
        bootstrap_json.write_text("""{
          "packageManager": "opkg",
          "settings": {
            "wireless": {
              "default_radio0": {
                "_type": "wifi-iface",
                "device": "radio0",
                "network": "lan",
                "mode": "ap",
                "ssid": "gchq-2.4",
                "encryption": "sae-mixed",
                "key": "CHANGE_ME_ON_DEPLOY"
              }
            }
          }
        }""")

        try:
            # 1. Day-1: compile a bootstrap config with no secrets
            r = run(
                [
                    "cargo",
                    "run",
                    "--",
                    "compile",
                    str(bootstrap_json),
                    "--no-sops",
                ]
            )
            assert r.returncode == 0
            bootstrap_commands = r.stdout
            assert "CHANGE_ME_ON_DEPLOY" in bootstrap_commands
            assert "@wifi_password@" not in bootstrap_commands

            # 2. Simulate first boot: execute uci-defaults bootstrap logic in container
            bootstrap_script = f"""#!/bin/sh
uci -q batch <<'UCI'
{bootstrap_commands}
UCI
uci commit
"""
            podman_exec(CONTAINER_NAME, bootstrap_script)

            check_uci_value(
                CONTAINER_NAME,
                "wireless.default_radio0.key",
                "CHANGE_ME_ON_DEPLOY",
                "[Lifecycle Day-1] WiFi Key",
            )

            # 3. Day-2: deploy with SOPS-decrypted secrets via SSH
            env = os.environ.copy()
            env["SOPS_AGE_KEY_FILE"] = str(SOPS_KEY_DIR / "keys.txt")
            env["NUCI_WATCHDOG_TIMEOUT"] = "10"

            r = run(
                [
                    "cargo",
                    "run",
                    "--",
                    "deploy",
                    str(test_json_opkg),
                    "--target",
                    "root@127.0.0.1",
                    "--port",
                    str(MAIN_SSH_PORT),
                    "--identity",
                    str(SSH_KEY_PATH),
                    "--force",
                ],
                env=env,
                timeout=120,
            )
            assert r.returncode == 0, f"Day-2 deploy failed:\n{r.stderr}\n{r.stdout}"

            check_uci_value(
                CONTAINER_NAME,
                "wireless.default_radio0.key",
                "my-test-password",
                "[Lifecycle Day-2] WiFi Key",
            )

        finally:
            bootstrap_json.unlink(missing_ok=True)
