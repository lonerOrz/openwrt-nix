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
# .ipk builder (opkg format: gzipped tar containing debian-binary +
#                data.tar.gz + control.tar.gz)
# ---------------------------------------------------------------------------


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
        with tarfile.open(data_tar, "w:gz", format=tarfile.GNU_FORMAT) as tf:
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
        with tarfile.open(control_tar, "w:gz", format=tarfile.GNU_FORMAT) as tf:
            tf.add(os.path.join(control_dir, "control"), arcname="./control")
            if conffiles:
                tf.add(os.path.join(control_dir, "conffiles"), arcname="./conffiles")

        # Assemble outer gzipped tar: debian-binary + data.tar.gz + control.tar.gz
        ipk_buf = io.BytesIO()
        with tarfile.open(
            fileobj=ipk_buf, mode="w:gz", format=tarfile.GNU_FORMAT
        ) as outer:
            # debian-binary
            db_info = tarfile.TarInfo(name="./debian-binary")
            db_bytes = b"2.0\n"
            db_info.size = len(db_bytes)
            outer.addfile(db_info, io.BytesIO(db_bytes))

            # data.tar.gz (already gzipped, add as-is)
            with open(data_tar, "rb") as df:
                data_bytes = df.read()
            data_info = tarfile.TarInfo(name="./data.tar.gz")
            data_info.size = len(data_bytes)
            outer.addfile(data_info, io.BytesIO(data_bytes))

            # control.tar.gz (already gzipped, add as-is)
            with open(control_tar, "rb") as cf:
                control_bytes = cf.read()
            ctrl_info = tarfile.TarInfo(name="./control.tar.gz")
            ctrl_info.size = len(control_bytes)
            outer.addfile(ctrl_info, io.BytesIO(control_bytes))

        return ipk_buf.getvalue()


# ---------------------------------------------------------------------------
# Package index generator
# ---------------------------------------------------------------------------


def generate_index_opkg(pkg_dir: Path) -> None:
    """Generate opkg Packages.gz index by parsing .ipk gzipped tar archives."""
    lines = []
    for ipk in sorted(pkg_dir.glob("*.ipk")):
        try:
            with tarfile.open(ipk, "r:gz") as outer:
                for member in outer.getmembers():
                    if member.name.endswith("control.tar.gz"):
                        ctrl_data = outer.extractfile(member)
                        if ctrl_data:
                            with tarfile.open(
                                fileobj=io.BytesIO(ctrl_data.read()), mode="r:*"
                            ) as ctf:
                                for cm in ctf.getmembers():
                                    if cm.name.endswith("control"):
                                        with ctf.extractfile(cm) as ctrl:
                                            lines.append(
                                                ctrl.read()
                                                .decode(errors="replace")
                                                .strip()
                                            )
                                            lines.append("")
        except (tarfile.TarError, OSError):
            continue

    index_content = "\n".join(lines).encode()
    with gzip.open(pkg_dir / "Packages.gz", "wb") as gz:
        gz.write(index_content)


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
