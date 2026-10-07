#!/usr/bin/env python3
"""Verify FFmpeg's matching source package before copying it into an App."""
import hashlib
import json
from pathlib import Path
import re
import sys


def verify(root):
    manifest = root / "source-manifest.json"
    if not manifest.is_file() or manifest.is_symlink():
        raise ValueError("FFmpeg corresponding source is missing; rebuild with scripts/ffmpeg/build-macos-arm64.sh")
    data = json.loads(manifest.read_text())
    # The generated manifest must agree with the repository's independently
    # reviewed source pin, not merely be internally self-consistent.
    build_script = Path(__file__).resolve().parents[1] / "ffmpeg/build-macos-arm64.sh"
    source_pin = re.search(r'^SHA256="([0-9a-f]{64})"$', build_script.read_text(), re.M).group(1)
    if data["archive_sha256"] != source_pin:
        raise ValueError("FFmpeg source does not match the repository's locked archive")
    binary_sha = hashlib.sha256((root / "ffmpeg").read_bytes()).hexdigest()
    if binary_sha != data["binary_sha256"]:
        raise ValueError("FFmpeg binary does not match its corresponding-source manifest")
    expected = set()
    for row in data["files"]:
        relative = Path(row["path"])
        if relative.is_absolute() or ".." in relative.parts or relative.parts[0] != "corresponding-source":
            raise ValueError("Invalid corresponding-source manifest path")
        path = root / relative
        if path.is_symlink() or not path.is_file():
            raise ValueError("Missing or non-regular FFmpeg corresponding-source file")
        content = path.read_bytes()
        if len(content) != row["size_bytes"] or hashlib.sha256(content).hexdigest() != row["sha256"]:
            raise ValueError("FFmpeg corresponding-source checksum mismatch")
        expected.add(relative.as_posix())
    required = {"corresponding-source/" + name for name in (
        data["archive"], "build-macos-arm64.sh", "BUILD-CONFIGURATION.txt", "README.txt")}
    if not required <= expected:
        raise ValueError("Incomplete FFmpeg corresponding-source package")
    archive = root / "corresponding-source" / data["archive"]
    if hashlib.sha256(archive.read_bytes()).hexdigest() != data["archive_sha256"]:
        raise ValueError("FFmpeg source archive checksum mismatch")
    actual = {p.relative_to(root).as_posix() for p in (root / "corresponding-source").rglob("*") if p.is_file()}
    if actual != expected:
        raise ValueError("Unlisted FFmpeg corresponding-source file")


if __name__ == "__main__":
    try:
        verify(Path(sys.argv[1]))
    except (ValueError, OSError, KeyError) as error:
        print(f"error: {error}", file=sys.stderr)
        sys.exit(1)
