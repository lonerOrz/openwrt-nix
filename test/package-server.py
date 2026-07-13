#!/usr/bin/env python3
"""
Minimal package server for integration tests.

Generates minimal .ipk/.apk packages and serves them via HTTP.
Usage:
    ./package-server.py --port 8080 --dir /tmp/test-packages
    # or with pre-built packages:
    ./package-server.py --port 8080 --dir ./test/test_packages --no-generate
"""

import argparse
import gzip
import http.server
import io
import os
import tarfile
import tempfile
import threading
from pathlib import Path

# ---------------------------------------------------------------------------
# .ipk builder (opkg format: ar archive containing data.tar.gz + control.tar.gz)
# ---------------------------------------------------------------------------


def _ar_header(name: str, size: int) -> bytes:
    """Generate a Unix ar archive header."""
    name_bytes = name.encode("ascii")[:16].ljust(16, b" ")
    size_str = str(size).encode("ascii").rjust(10, b" ")
    # ar header: name(16) + timestamp(12) + uid(6) + gid(6) + mode(8) + size(10) + magic(2) = 60
    return (
        name_bytes
        + b"0           "
        + b"0     "
        + b"0     "
        + b"100644  "
        + size_str
        + b"`\n"
    )


def build_ipk(
    package: str,
    version: str,
    description: str = "",
    depends: str = "",
    install_script: str = "",
    conffiles: list[str] | None = None,
) -> bytes:
    """Build a minimal .ipk (opkg package) in memory."""
    with tempfile.TemporaryDirectory() as tmp:
        # data.tar.gz — empty payload (or install script)
        data_tar = os.path.join(tmp, "data.tar.gz")
        with tarfile.open(data_tar, "w:gz") as tf:
            if install_script:
                info = tarfile.TarInfo(name="./usr/share/test_marker")
                marker = install_script.encode()
                info.size = len(marker)
                tf.addfile(info, io.BytesIO(marker))

        # control.tar.gz
        control_dir = os.path.join(tmp, "control")
        os.makedirs(control_dir)
        control_lines = [
            f"Package: {package}",
            f"Version: {version}",
            "Architecture: all",
            "Maintainer: Test <test@test.local>",
            f"Depends: {depends}" if depends else None,
            f"Description: {description or package}",
            "Installed-Size: 0",
            "",
        ]
        control_lines = [line for line in control_lines if line is not None]
        with open(os.path.join(control_dir, "control"), "w") as f:
            f.write("\n".join(control_lines) + "\n")
        if conffiles:
            with open(os.path.join(control_dir, "conffiles"), "w") as f:
                f.write("\n".join(conffiles) + "\n")

        control_tar = os.path.join(tmp, "control.tar.gz")
        with tarfile.open(control_tar, "w:gz") as tf:
            tf.add(os.path.join(control_dir, "control"), arcname="./control")
            if conffiles:
                tf.add(os.path.join(control_dir, "conffiles"), arcname="./conffiles")

        # Assemble: debian-binary + data.tar.gz + control.tar.gz
        db_str = b"2.0\n"
        with open(data_tar, "rb") as df:
            data_bytes = df.read()
        with open(control_tar, "rb") as cf:
            control_bytes = cf.read()

        ar_magic = b"!<arch>\n"
        parts = [
            ar_magic,
            _ar_header("debian-binary", len(db_str)),
            db_str,
            _ar_header("data.tar.gz", len(data_bytes)),
            data_bytes,
            _ar_header("control.tar.gz", len(control_bytes)),
            control_bytes,
        ]
        # Pad to 2-byte boundary
        ipk = b"".join(parts)
        if len(ipk) % 2:
            ipk += b"\n"
        return ipk


# ---------------------------------------------------------------------------
# .apk builder (Alpine package format: tar.gz with .PKGINFO + data.tar.gz)
# ---------------------------------------------------------------------------


def build_apk(package: str, version: str, description: str = "") -> bytes:
    """Build a minimal .apk (Alpine package) in memory."""
    pkginfo = (
        f"pkgname = {package}\n"
        f"pkgver = {version}\n"
        f"arch = noarch\n"
        f"size = 0\n"
        f"url = http://localhost\n"
        f"license = MIT\n"
        f"description = {description or package}\n"
    )

    with tempfile.NamedTemporaryFile(suffix=".apk", delete=False) as tmp:
        with tarfile.open(tmp.name, "w:gz") as tf:
            info = tarfile.TarInfo(name=".PKGINFO")
            data = pkginfo.encode()
            info.size = len(data)
            tf.addfile(info, io.BytesIO(data))
        with open(tmp.name, "rb") as f:
            result = f.read()
        os.unlink(tmp.name)
    return result


# ---------------------------------------------------------------------------
# Package index generator
# ---------------------------------------------------------------------------


def generate_index_opkg(pkg_dir: Path) -> None:
    """Generate opkg Packages.gz index by parsing .ipk ar archives."""
    lines = []
    for ipk in sorted(pkg_dir.glob("*.ipk")):
        # .ipk is an ar archive: !<arch>\n + entries
        # Each entry: 16-byte name + metadata + data
        with open(ipk, "rb") as f:
            magic = f.read(8)
            if magic != b"!<arch>\n":
                continue
            while True:
                header = f.read(60)
                if len(header) < 60:
                    break
                name = header[:16].decode("ascii").strip()
                size = int(header[48:58].decode("ascii").strip())
                data = f.read(size)
                if len(data) < size:
                    break
                if name == "control.tar.gz":
                    import io as _io

                    with tarfile.open(fileobj=_io.BytesIO(data), mode="r:*") as ctf:
                        for cm in ctf.getmembers():
                            if cm.name.endswith("control"):
                                with ctf.extractfile(cm) as ctrl:
                                    lines.append(
                                        ctrl.read().decode(errors="replace").strip()
                                    )
                                    lines.append("")

    index_content = "\n".join(lines).encode()
    with gzip.open(pkg_dir / "Packages.gz", "wb") as gz:
        gz.write(index_content)


def generate_index_apk(pkg_dir: Path) -> None:
    """Generate apk index."""
    for apk_file in sorted(pkg_dir.glob("*.apk")):
        pass  # apk discovers packages by scanning the directory


# ---------------------------------------------------------------------------
# HTTP server
# ---------------------------------------------------------------------------


class PackageHandler(http.server.SimpleHTTPRequestHandler):
    package_dir: str = ""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=self.package_dir, **kwargs)

    def end_headers(self):
        # CORS headers for cross-origin requests
        self.send_header("Access-Control-Allow-Origin", "*")
        super().end_headers()

    def log_message(self, format, *args):
        pass  # Suppress request logging in tests


def start_server(directory: str, port: int = 8080) -> http.server.HTTPServer:
    """Start the package server in a background thread."""
    PackageHandler.package_dir = directory
    server = http.server.HTTPServer(("0.0.0.0", port), PackageHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main():
    parser = argparse.ArgumentParser(description="Package server for integration tests")
    parser.add_argument("--port", type=int, default=8080, help="Port to serve on")
    parser.add_argument("--dir", required=True, help="Directory to serve packages from")
    parser.add_argument(
        "--no-generate", action="store_true", help="Don't generate test packages"
    )
    args = parser.parse_args()

    pkg_dir = Path(args.dir)
    pkg_dir.mkdir(parents=True, exist_ok=True)

    if not args.no_generate:
        # Generate test packages
        print(f"Generating test packages in {pkg_dir}...")

        # opkg packages
        for name, ver, deps in [
            ("test-pkg-a", "1.0-r1", ""),
            ("test-pkg-b", "2.0-r1", "test-pkg-a"),
            ("luci-app-test", "0.1-r1", "test-pkg-a"),
        ]:
            ipk = build_ipk(name, ver, description=f"Test package {name}", depends=deps)
            (pkg_dir / f"{name}_{ver}_all.ipk").write_bytes(ipk)

        # apk packages
        for name, ver in [
            ("test-pkg-a", "1.0-r1"),
            ("test-pkg-b", "2.0-r1"),
        ]:
            apk = build_apk(name, ver, description=f"Test package {name}")
            (pkg_dir / f"{name}-{ver}.apk").write_bytes(apk)

        # Generate indices
        generate_index_opkg(pkg_dir)
        print(f"Generated {len(list(pkg_dir.glob('*')))} files")

    print(f"Package server listening on http://0.0.0.0:{args.port}")
    server = start_server(str(pkg_dir), args.port)

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.shutdown()


if __name__ == "__main__":
    main()
