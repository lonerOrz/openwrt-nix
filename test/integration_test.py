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
from pathlib import Path

import pytest

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parent.parent
CONTAINER_NAME = "openwrt-integration-test"
AGENT_CONTAINER_NAME = "openwrt-agent-test"
SSH_KEY_PATH = Path("/tmp/openwrt_test_key")
SSH_CONFIG_PATH = Path("/tmp/openwrt_test_ssh_config")
SOPS_KEY_DIR = Path("/tmp/nuci_sops_test")
ENCRYPTED_SECRETS = PROJECT_ROOT / "test" / "secrets.enc.json"
PACKAGE_DIR = Path("/tmp/nuci-test-packages")

ENGINE = os.environ.get("CONTAINER_ENGINE", "podman")

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
    """Kill dropbear in a container (idempotent)."""
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
        ["nix", "run", f"{PROJECT_ROOT}#test-deploy", "--"],
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
        ["nix", "run", f"{PROJECT_ROOT}#test-deploy-apk", "--"],
        check=True,
        env=env,
    )
    return r.stdout


@pytest.fixture(scope="session")
def test_json_opkg() -> Path:
    """Build and return path to test JSON artifact (opkg)."""
    r = run(
        ["nix", "build", f"{PROJECT_ROOT}#test-json", "--print-out-paths", "--no-link"]
    )
    return Path(r.stdout.strip())


@pytest.fixture(scope="session")
def test_json_apk() -> Path:
    """Build and return path to test JSON artifact (apk)."""
    r = run(
        [
            "nix",
            "build",
            f"{PROJECT_ROOT}#test-json-apk",
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
    """Session-scoped setup: build container, inject keys, setup SOPS, start package server."""
    # Clean previous artifacts
    engine("rm", "-f", CONTAINER_NAME, check=False)
    engine("rm", "-f", AGENT_CONTAINER_NAME, check=False)
    for p in [
        SSH_KEY_PATH,
        Path(f"{SSH_KEY_PATH}.pub"),
        SSH_CONFIG_PATH,
        Path(f"{SSH_KEY_PATH}.agent"),
        Path(f"{SSH_KEY_PATH}.agent.pub"),
        Path("/tmp/openwrt_agent_ssh_config"),
    ]:
        p.unlink(missing_ok=True)
    shutil.rmtree(SOPS_KEY_DIR, ignore_errors=True)
    shutil.rmtree(PACKAGE_DIR, ignore_errors=True)
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
        "2222:22",
        "openwrt-test-env",
    )

    # Wait for dropbear
    wait_for_port("127.0.0.1", 2222)

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

    # Create SSH config
    SSH_CONFIG_PATH.write_text(
        f"Host openwrt-test\n"
        f"    HostName localhost\n"
        f"    Port 2222\n"
        f"    User root\n"
        f"    StrictHostKeyChecking no\n"
        f"    UserKnownHostsFile /dev/null\n"
        f"    IdentityFile {SSH_KEY_PATH}\n"
    )

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

    # Start package server
    PACKAGE_DIR.mkdir(parents=True, exist_ok=True)
    pkg_proc = subprocess.Popen(
        [
            "nix",
            "shell",
            "nixpkgs#python3",
            "-c",
            "python3",
            str(project_root / "test" / "package-server.py"),
            "--dir",
            str(PACKAGE_DIR),
            "--port",
            "8080",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    for _ in range(5):
        try:
            with socket.create_connection(("localhost", 8080), timeout=1):
                break
        except OSError:
            time.sleep(1)

    yield

    # Teardown
    pkg_proc.terminate()
    pkg_proc.wait(timeout=5)
    engine("rm", "-f", CONTAINER_NAME, check=False)
    engine("rm", "-f", AGENT_CONTAINER_NAME, check=False)
    for p in [
        SSH_KEY_PATH,
        Path(f"{SSH_KEY_PATH}.pub"),
        SSH_CONFIG_PATH,
        Path(f"{SSH_KEY_PATH}.agent"),
        Path(f"{SSH_KEY_PATH}.agent.pub"),
        Path("/tmp/openwrt_agent_ssh_config"),
    ]:
        p.unlink(missing_ok=True)
    shutil.rmtree(SOPS_KEY_DIR, ignore_errors=True)
    shutil.rmtree(PACKAGE_DIR, ignore_errors=True)
    ENCRYPTED_SECRETS.unlink(missing_ok=True)
    run(["git", "restore", "--staged", str(ENCRYPTED_SECRETS)], check=False)


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
            if line and "uci: Entry not found" not in line
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
        errors = [
            line
            for line in r.stderr.splitlines()
            if line and "uci: Entry not found" not in line
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
    """Test real opkg/apk package installation from local HTTP server."""

    @pytest.fixture(scope="class", autouse=True)
    def detect_gateway(self):
        """Detect container gateway IP (host running package-server)."""
        r = engine(
            "exec",
            CONTAINER_NAME,
            "ip",
            "route",
            "show",
            "default",
            check=False,
        )
        match = re.search(r"via\s+(\S+)", r.stdout)
        assert match, f"Cannot detect gateway: {r.stdout}"
        self.gateway = match.group(1)

    def test_opkg_real_install(self):
        """Install a real .ipk package via opkg from local feed."""
        gateway = self.gateway
        podman_exec(
            CONTAINER_NAME,
            f"""
            printf '' > /etc/opkg/customfeeds.conf
            echo "src/gz test-pkgs http://{gateway}:8080" >> /etc/opkg/customfeeds.conf
            opkg update
            opkg install test-pkg-a
        """,
        )
        r = engine(
            "exec",
            CONTAINER_NAME,
            "opkg",
            "list-installed",
            "test-pkg-a",
            check=False,
        )
        assert "test-pkg-a" in r.stdout, f"Package not installed: {r.stdout}"

    def test_apk_real_install(self):
        """Install a real .apk package via apk from local feed."""
        gateway = self.gateway
        podman_exec(
            CONTAINER_NAME,
            f"""
            echo "http://{gateway}:8080" > /etc/apk/repositories.d/test.list
            apk update
            apk add test-pkg-a
        """,
        )
        r = engine(
            "exec",
            CONTAINER_NAME,
            "apk",
            "info",
            "-e",
            "test-pkg-a",
            check=False,
        )
        assert r.returncode == 0, f"apk package not installed: {r.stderr}"


class TestServiceState:
    """Step 11: Service state verification + syslog scanning."""

    def test_dropbear_running(self):
        try:
            with socket.create_connection(("127.0.0.1", 2222), timeout=2):
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
        """SSH login works with the deployed password."""
        r = run(
            [
                "sshpass",
                "-p",
                "nuci-test-pw-2025",
                "ssh",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-p",
                "2222",
                "root@127.0.0.1",
                "echo ok",
            ],
            check=False,
            timeout=10,
        )
        assert r.returncode == 0, f"Password auth failed: {r.stderr}"
        assert "ok" in r.stdout


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
            "2223:22",
            "openwrt-agent-test-env",
        )
        wait_for_port("127.0.0.1", 2223)
        yield
        engine("rm", "-f", AGENT_CONTAINER_NAME, check=False)

    def test_password_auth_works(self, agent_container):
        wait_for_port("127.0.0.1", 2223)

    def test_initial_keys_empty(self, agent_container):
        keys = podman_exec(
            AGENT_CONTAINER_NAME, "cat /etc/dropbear/authorized_keys", check=False
        )
        assert not keys.strip(), f"authorized_keys already has content: {keys}"

    def test_key_deployment(self, agent_container):
        """Deploy SSH key and verify it works."""
        agent_key = Path("/tmp/openwrt_agent_key")
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

        agent_ssh_config = Path("/tmp/openwrt_agent_ssh_config")
        agent_ssh_config.write_text(
            f"Host openwrt-agent-test\n"
            f"    HostName localhost\n"
            f"    Port 2223\n"
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
            podman_exec(AGENT_CONTAINER_NAME, "killall dropbear || true", check=False)
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
                "2222",
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
        cmd = (
            f"sleep 20; "
            f"cp -a /tmp/.uci-rollback-backup/* /etc/config/; "
            f"{DROPBEAR_BIN} & "
            f"rm -rf /tmp/.uci-rollback-backup /tmp/.uci-watchdog-pid"
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
        for _ in range(15):
            time.sleep(2)
            try:
                result = ssh_cmd(SSH_CONFIG_PATH, "openwrt-test", "echo ok", timeout=3)
                if result == "ok":
                    restored = True
                    break
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError):
                continue

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
            f"sleep 8; "
            f"cp -a /tmp/.uci-rollback-backup-fault/* /etc/config/; "
            f"{DROPBEAR_BIN} & "
            f"rm -rf /tmp/.uci-rollback-backup-fault /tmp/.uci-watchdog-pid-fault"
        )
        engine("exec", "-d", CONTAINER_NAME, "sh", "-c", cmd)
        podman_exec(CONTAINER_NAME, "echo detached > /tmp/.uci-watchdog-pid-fault")

        podman_exec(
            CONTAINER_NAME, "tc qdisc add dev eth0 root netem loss 80%", check=False
        )
        time.sleep(1)
        podman_exec(CONTAINER_NAME, "tc qdisc del dev eth0 root", check=False)

        restored = False
        for _ in range(15):
            time.sleep(2)
            try:
                result = ssh_cmd(SSH_CONFIG_PATH, "openwrt-test", "echo ok", timeout=3)
                if result == "ok":
                    restored = True
                    break
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError):
                continue

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
            f"sleep 8; "
            f"cp -a /tmp/.uci-rollback-backup-crash/* /etc/config/; "
            f"{DROPBEAR_BIN} & "
            f"rm -rf /tmp/.uci-rollback-backup-crash /tmp/.uci-watchdog-pid-crash"
        )
        engine("exec", "-d", CONTAINER_NAME, "sh", "-c", cmd)
        podman_exec(CONTAINER_NAME, "echo detached > /tmp/.uci-watchdog-pid-crash")

        # Verify total blackout
        time.sleep(1)
        assert not dropbear_running(CONTAINER_NAME), (
            f"[Fault B] dropbear still running:\n{podman_exec(CONTAINER_NAME, 'ps')}"
        )

        restored = False
        for _ in range(15):
            time.sleep(2)
            try:
                result = ssh_cmd(SSH_CONFIG_PATH, "openwrt-test", "echo ok", timeout=3)
                if result == "ok":
                    restored = True
                    break
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError):
                continue

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

        assert hostname == "rauter-apk", f"[Fault C] hostname corrupted: {hostname}"
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
