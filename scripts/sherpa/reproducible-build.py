#!/usr/bin/env python3
"""Normalize embedded native paths and enforce the reviewed artifact toolchain."""
import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys


def prefix_flags(onnxruntime, sherpa, build, scripts):
    mappings = [(onnxruntime, "/seasnail/onnxruntime"),
                (sherpa, "/seasnail/sherpa-onnx"),
                (build, "/seasnail/build"),
                (scripts, "/seasnail/scripts")]
    return [f"-ffile-prefix-map={Path(source).resolve()}={canonical}"
            for source, canonical in mappings] + ["-fdebug-compilation-dir=/seasnail"]


def check_toolchain():
    expected = json.loads(Path(__file__).with_name("native-toolchain.json").read_text())
    commands = {"clang": ["cc", "--version"],
                "clangxx": ["c++", "--version"],
                "sdk_version": ["xcrun", "--show-sdk-version"],
                "sdk_build": ["xcrun", "--show-sdk-build-version"],
                "cmake": ["cmake", "--version"]}
    mismatches = []
    for variable in ("CC", "CXX", "CFLAGS", "CXXFLAGS", "LDFLAGS"):
        if os.environ.get(variable):
            mismatches.append(f"Unset {variable} for the locked native build")
    for key, command in commands.items():
        actual = subprocess.check_output(command, text=True).splitlines()[0].strip()
        if key == "cmake":
            actual = actual.removeprefix("cmake version ")
        wanted = expected["clang" if key == "clangxx" else key]
        if actual != wanted:
            mismatches.append(f"{key}: expected {wanted!r}, got {actual!r}")
    if mismatches:
        raise ValueError("Native artifact toolchain mismatch:\n" + "\n".join(mismatches)
                         + "\nSelect the locked tools/SDK or use a verified matching artifact. "
                         "Do not change the artifact hashes to bypass this check.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-toolchain", action="store_true")
    parser.add_argument("paths", nargs="*")
    args = parser.parse_args()
    if args.check_toolchain:
        check_toolchain()
    else:
        if len(args.paths) != 4:
            parser.error("Expected ONNXRUNTIME_SOURCE SHERPA_SOURCE BUILD_ROOT SCRIPT_ROOT")
        print(shlex.join(prefix_flags(*args.paths)))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
