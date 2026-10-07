#!/usr/bin/env python3
"""Check DCO trailers on the commits introduced by a pull request."""
import re
import subprocess
import sys

if len(sys.argv) != 3:
    sys.exit("Usage: python3 scripts/check-dco.py BASE_SHA HEAD_SHA")
base, head = sys.argv[1:]
if not all(re.fullmatch(r"[0-9a-f]{40,64}", sha) for sha in (base, head)):
    sys.exit("Expected full Git commit hashes")
commits = subprocess.check_output(["git", "rev-list", f"{base}..{head}"], text=True).splitlines()
failed = []
for commit in commits:
    message = subprocess.check_output(["git", "show", "-s", "--format=%B", commit], text=True)
    trailers = subprocess.check_output(["git", "interpret-trailers", "--parse"], input=message, text=True)
    if not re.search(r"^Signed-off-by: .+ <[^<>\s]+@[^<>\s]+>$", trailers, re.MULTILINE | re.IGNORECASE):
        failed.append(commit)
if failed:
    sys.exit("Missing valid DCO sign-off: " + ", ".join(failed) + ". See CONTRIBUTING.md.")
print(f"DCO passed for {len(commits)} commits")
