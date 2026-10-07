#!/usr/bin/env python3
"""Fetch locked upstream sources and archives; never replace mismatched existing inputs."""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

SCRIPT_DIR = Path(__file__).resolve().parent


def verify(path, row):
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"Not a regular file: {path}")
    if path.stat().st_size != row["size_bytes"]:
        raise ValueError(f"Size mismatch: {path}; move the file aside and retry")
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    if digest.hexdigest() != row["sha256"]:
        raise ValueError(f"SHA-256 mismatch: {path}; move the file aside and retry")


def download(cache, row):
    name = row["file_name"]
    if Path(name).name != name or not row["url"].startswith("https://"):
        raise ValueError("Invalid locked filename or URL")
    target = cache / name
    if target.exists() or target.is_symlink():
        verify(target, row)
        print(f"Verified existing {name}", flush=True)
        return
    print(f"Downloading {name} ({row['size_bytes']} bytes)", flush=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=cache, prefix=f".{name}.", delete=False) as output:
            temporary = Path(output.name)
        # macOS curl uses the system trust configuration; Python installations
        # can have an uninitialized private CA bundle. Never disable TLS checks.
        subprocess.run([
            "curl", "--fail", "--silent", "--show-error", "--location",
            "--proto", "=https", "--proto-redir", "=https",
            "--connect-timeout", "30", "--max-time", "600",
            "--max-filesize", str(row["size_bytes"]),
            "--output", str(temporary), "--url", row["url"],
        ], check=True)
        verify(temporary, row)
        # No overwrite if another preparation process published this file.
        try:
            os.link(temporary, target)
        except FileExistsError:
            verify(target, row)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def git(path, *arguments, capture=False):
    return subprocess.run(["git", "-C", str(path), *arguments], check=True,
                          text=True, timeout=300, stdout=subprocess.PIPE if capture else None).stdout


def checkout(sources, name, row):
    if not row["repository"].startswith("https://") or not re.fullmatch(r"[0-9a-f]{40}", row["revision"]):
        raise ValueError("Sources require an HTTPS repository and a full pinned commit")
    target = sources / name
    if target.exists() or target.is_symlink():
        if target.is_symlink() or not (target / ".git").exists():
            raise ValueError(f"Not a source checkout: {target}")
        if git(target, "rev-parse", "HEAD", capture=True).strip() != row["revision"]:
            raise ValueError(f"Wrong revision: {target}; move it aside and retry")
        if git(target, "status", "--porcelain", "--untracked-files=normal", capture=True).strip():
            raise ValueError(f"Dirty checkout: {target}; changes will not be overwritten")
        for submodule in row.get("submodules", []):
            if git(target / submodule["path"], "rev-parse", "HEAD", capture=True).strip() != submodule["revision"]:
                raise ValueError(f"Submodule revision mismatch: {submodule['path']}")
        print(f"Verified existing {name}", flush=True)
        return
    # Clone in a temporary sibling so interrupted downloads cannot poison reuse.
    temporary = Path(tempfile.mkdtemp(dir=sources, prefix=f".{name}."))
    try:
        subprocess.run(["git", "clone", "--depth", "1", "--branch", row["tag"], "--filter=blob:none", "--no-checkout", "--", row["repository"], str(temporary)], check=True, timeout=300)
        if git(temporary, "rev-parse", "HEAD", capture=True).strip() != row["revision"]:
            raise ValueError(f"Upstream tag no longer matches the locked commit: {name}")
        if name == "onnxruntime":
            # Unit tests are disabled in the locked production build. Avoid
            # fetching large upstream test models; all build sources stay pinned.
            git(temporary, "sparse-checkout", "set", "--no-cone", "/*", "!/onnxruntime/test/testdata/")
        git(temporary, "checkout", "--detach", row["revision"])
        for submodule in row.get("submodules", []):
            git(temporary, "submodule", "update", "--init", "--recursive", "--depth", "1", "--", submodule["path"])
        temporary.rename(target)
    finally:
        if temporary.exists():
            shutil.rmtree(temporary)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=SCRIPT_DIR.parents[1] / "third_party/sherpa/macos-arm64")
    parser.add_argument("--list", action="store_true", help="Print locked inputs without downloading")
    args = parser.parse_args()
    source = json.loads((SCRIPT_DIR / "source-lock.json").read_text())
    artifact = json.loads((SCRIPT_DIR / "artifact-lock.json").read_text())
    downloads = source["source_archives"] + source["onnxruntime_cmake_dependencies"] + artifact["downloads"]
    if args.list:
        print(json.dumps({"sources": [{"name": name, "repository": source[name]["repository"], "revision": source[name]["revision"]} for name in ("sherpa_onnx", "onnxruntime")], "downloads": downloads}, indent=2))
        return 0
    root = args.root.resolve()
    cache, sources = root / "cache", root / "sources"
    cache.mkdir(parents=True, exist_ok=True)
    sources.mkdir(parents=True, exist_ok=True)
    for row in downloads:
        download(cache, row)
    checkout(sources, "sherpa-onnx", source["sherpa_onnx"])
    checkout(sources, "onnxruntime", source["onnxruntime"])
    subprocess.run(["bash", str(SCRIPT_DIR / "verify-locks.sh"), "--cache", str(cache), "--source", str(sources / "sherpa-onnx"), "--onnxruntime-source", str(sources / "onnxruntime")], check=True)
    print("Locked inputs verified. Continue with scripts/build.sh --package.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        print(f"Input preparation failed: {error}", file=sys.stderr)
        sys.exit(1)
