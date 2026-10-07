#!/usr/bin/env python3
"""Read-only prerequisite checks for first-time SeaSnail development."""
import argparse
import platform
from pathlib import Path
import re
import shutil
import subprocess
import sys


def supported_node(version):
    match = re.search(r"v?(\d+)\.(\d+)\.(\d+)", version)
    if not match:
        return False
    major, minor, patch = map(int, match.groups())
    return ((major == 22 and (minor, patch) >= (22, 2))
            or (major == 24 and minor >= 15) or major >= 26)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--frontend-only", action="store_true", help="Check UI development prerequisites only")
    args = parser.parse_args()
    failures = []
    if not args.frontend_only and (platform.system() != "Darwin" or platform.machine() != "arm64"):
        failures.append("Native app builds require macOS 13+ on Apple Silicon (arm64).")
    if not args.frontend_only and platform.system() == "Darwin":
        version = platform.mac_ver()[0]
        if not version or int(version.split(".")[0]) < 13:
            failures.append("macOS 13 or newer is required.")
    commands = {"git": ["--version"], "node": ["--version"], "pnpm": ["--version"]}
    if not args.frontend_only:
        commands.update({"cargo": ["--version"], "rustc": ["--version"], "jq": ["--version"], "cmake": ["--version"], "xcode-select": ["-p"], "xcrun": ["--find", "clang"], "curl": ["--version"]})
    for name, arguments in commands.items():
        if not shutil.which(name):
            hint = ' Run: source "$HOME/.cargo/env" (if rustup is installed).' if name in ("cargo", "rustc") else ""
            failures.append(f"Missing {name}.{hint}")
            continue
        result = subprocess.run([name, *arguments], capture_output=True, text=True)
        output = result.stdout.strip().splitlines()
        if result.returncode or not output:
            failures.append(f"{name} is installed but its prerequisite check failed.")
            continue
        print(f"OK {name}: {output[0]}")
        if name == "node" and not supported_node(output[0]):
            failures.append("Node.js must be 22.x >=22.22.2, 24.x >=24.15.0, or >=26.0.0. Select a supported version and reopen the terminal.")
        if name == "pnpm":
            match = re.match(r"(\d+)", output[0])
            if not match or int(match.group(1)) < 9:
                failures.append("pnpm 9 or newer is required.")
    if not args.frontend_only and sys.version_info < (3, 11):
        failures.append("Python 3.11 or newer is required for App dependency-license collection.")
    else:
        print(f"OK python: {platform.python_version()}")
    if not args.frontend_only and not failures:
        result = subprocess.run([sys.executable, str(Path(__file__).resolve().parent / "sherpa/reproducible-build.py"),
                                 "--check-toolchain"], capture_output=True, text=True)
        if result.returncode:
            failures.append(result.stderr.strip() or "Native artifact toolchain check failed.")
        else:
            print("OK native artifact toolchain: scripts/sherpa/native-toolchain.json")
    for failure in failures:
        print(f"FAIL {failure}", file=sys.stderr)
    if failures:
        print("See doc/development.md for setup instructions.", file=sys.stderr)
        return 1
    print("Prerequisites passed. Runtime downloads, artifact verification, and system permissions are separate checks.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
