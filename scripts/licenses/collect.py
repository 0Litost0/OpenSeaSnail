#!/usr/bin/env python3
"""Collect offline, version-specific App notices; fail on missing license evidence."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
try:
    import tomllib
except ModuleNotFoundError:
    raise SystemExit("Python 3.11 or newer is required to collect Cargo license metadata.")

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
LICENSE_NAME = re.compile(r"^(licen[cs]e|copying|notice|copyright)(?:[._-].*|$)", re.I)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def checked(data, row):
    if len(data) != row["size_bytes"] or digest(data) != row["sha256"]:
        raise ValueError(f"License material checksum mismatch: {row.get('path', row.get('name'))}")
    return data


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def package_files(directory):
    """Retain package-owned notices, including license directories."""
    files = {}
    for path in sorted(directory.rglob("*")):
        relative = path.relative_to(directory)
        if "node_modules" in relative.parts or not path.is_file():
            continue
        if any(LICENSE_NAME.match(part) for part in relative.parts):
            files[relative.as_posix()] = path.read_bytes()
    return files


def supplements(ecosystem, name, version, materials):
    files = {}
    for row in materials["dependency_supplements"]:
        if row["ecosystem"] == ecosystem and row["name"] == name and row["version"] == version:
            for entry in row["files"]:
                files[Path(entry["path"]).name] = checked((ROOT / entry["path"]).read_bytes(), entry)
    return files


def rust_packages(materials, cargo_home):
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    locked = {(p["name"], p["version"]): p for p in lock["package"]}
    tree = subprocess.check_output([
        "cargo", "tree", "--locked", "--offline", "--target", "aarch64-apple-darwin",
        "--edges", "normal,build", "--prefix", "none", "--format", "{p}",
    ], cwd=ROOT, text=True)
    identities = sorted(set(re.findall(r"^(\S+) v([^\s]+)", tree, re.M)))
    for name, version in identities:
        row = locked[(name, version)]
        source = row.get("source", "")
        if not source:
            continue  # SeaSnail workspace code uses the root LICENSE/NOTICE.
        archive_data = None
        identity = f"{name}-{version}"
        if source.startswith("registry+"):
            candidates = sorted(cargo_home.glob(f"registry/cache/*/{identity}.crate"))
            archive = next((p for p in candidates if digest(p.read_bytes()) == row["checksum"]), None)
            if archive is None:
                raise ValueError(f"Missing checksum-verified crate archive: {identity}; run cargo fetch --target aarch64-apple-darwin --locked")
            archive_data = archive.read_bytes()
            with tarfile.open(archive) as tar:
                prefix = identity + "/"
                pkg = tomllib.loads(tar.extractfile(prefix + "Cargo.toml").read().decode())["package"]
                files = {}
                for member in tar.getmembers():
                    if not member.isfile() or not member.name.startswith(prefix):
                        continue
                    relative = member.name[len(prefix):]
                    if any(LICENSE_NAME.match(part) for part in Path(relative).parts):
                        files[relative] = tar.extractfile(member).read()
        elif source.startswith("git+"):
            revision = source.rsplit("#", 1)[-1]
            checkout = None
            for path in sorted(cargo_home.glob(f"git/checkouts/{name}-*/*")):
                actual = subprocess.check_output(["git", "-C", str(path), "rev-parse", "HEAD"], text=True).strip()
                if actual == revision:
                    dirty = subprocess.check_output(["git", "-C", str(path), "status", "--porcelain", "--untracked-files=no"], text=True)
                    if dirty:
                        raise ValueError(f"Modified dependency checkout: {name}")
                    checkout = path
                    break
            if checkout is None:
                raise ValueError(f"Missing locked Git checkout: {name}@{revision}")
            pkg = tomllib.loads(subprocess.check_output([
                "git", "-C", str(checkout), "show", revision + ":Cargo.toml",
            ], text=True))["package"]
            # Read pinned Git objects, never untracked cache-local notices.
            names = subprocess.check_output([
                "git", "-C", str(checkout), "ls-tree", "-rz", "--name-only", revision,
            ]).decode().split("\0")
            files = {name: subprocess.check_output([
                "git", "-C", str(checkout), "show", revision + ":" + name,
            ]) for name in names if name and any(LICENSE_NAME.match(part) for part in Path(name).parts)}
        else:
            raise ValueError(f"Unsupported Cargo source: {name}")
        files.update(supplements("cargo", name, version, materials))
        license_id = pkg.get("license")
        if (not license_id and (name, version) == ("tauri-nspanel", "2.1.0")
                and source.endswith("#c9ec2130422200f0863b23dfdad02b133a529b07")):
            # The pinned checkout ships both full texts but omits Cargo metadata.
            license_id = "MIT OR Apache-2.0"
        if not files or not license_id:
            raise ValueError(f"Missing license text or identifier: {name}@{version}")
        yield {"ecosystem": "cargo", "name": name, "version": version,
               "license": license_id, "source": source,
               "authors": pkg.get("authors", []), "checksum": row.get("checksum")}, files, archive_data


def javascript_packages(materials):
    frontend = ROOT / "apps/desktop"
    wanted = json.loads((frontend / "package.json").read_text())["dependencies"]
    installed = json.loads(subprocess.check_output([
        "pnpm", "--dir", str(frontend), "list", "--prod", "--depth", "Infinity", "--json",
    ], text=True))
    if not isinstance(installed, list) or len(installed) != 1:
        raise ValueError("Cannot read the installed frontend dependency tree")
    roots = installed[0].get("dependencies", {})
    if set(wanted) - set(roots):
        raise ValueError("Frontend dependencies missing; run pnpm install --frozen-lockfile")
    # pnpm list may include stale direct dependencies. Only traverse current roots.
    seen = {}

    def walk(name, row):
        identity = (name, row["version"])
        if identity in seen:
            return
        seen[identity] = row
        for section in ("dependencies", "optionalDependencies"):
            for child, dependency in row.get(section, {}).items():
                walk(child, dependency)

    for name in sorted(wanted):
        walk(name, roots[name])
    lock_text = (frontend / "pnpm-lock.yaml").read_text()
    locked = set(re.findall(r"^  ['\"]?((?:@[^/]+/)?[^@:'\"\s]+)@([^:'\"(\s]+)", lock_text, re.M))
    for (name, version), row in sorted(seen.items()):
        if (name, version) not in locked:
            raise ValueError(f"Installed frontend package is not locked: {name}@{version}")
        directory = Path(row["path"])
        pkg = json.loads((directory / "package.json").read_text())
        if (pkg["name"], pkg["version"]) != (name, version):
            raise ValueError(f"Installed package identity mismatch: {name}@{version}")
        files = package_files(directory)
        files.update(supplements("npm", name, version, materials))
        license_id = pkg.get("license")
        if not license_id or not files:
            raise ValueError(f"Missing license text or identifier: {name}@{version}")
        yield {"ecosystem": "npm", "name": name, "version": version,
               "license": license_id, "source": row.get("resolved", ""),
               "author": pkg.get("author", "")}, files, None


def swift_packages(materials):
    resolved = ROOT / "apps/desktop/native/post-paste-monitor/Package.resolved"
    for pin in json.loads(resolved.read_text())["pins"]:
        name, version = pin["identity"], pin["state"]["version"]
        rows = [row for row in materials["dependency_supplements"]
                if row["ecosystem"] == "swift" and row["name"] == name and row["version"] == version]
        if len(rows) != 1 or rows[0]["revision"] != pin["state"]["revision"]:
            raise ValueError(f"Missing revision-specific Swift license: {name}@{version}")
        files = supplements("swift", name, version, materials)
        yield {"ecosystem": "swift", "name": name, "version": version,
               "license": rows[0]["license"], "source": pin["location"],
               "revision": pin["state"]["revision"], "authors": rows[0]["authors"]}, files, None


def collect(output, cargo_home, native_cache=None):
    if output.exists():
        raise ValueError("Output must not already exist")
    materials = json.loads((HERE / "materials.json").read_text())
    # Validate every supplement, including entries not active on this platform.
    for row in materials["files"]:
        checked((ROOT / row["path"]).read_bytes(), row)
    packages = list(rust_packages(materials, cargo_home)) + list(javascript_packages(materials)) + list(swift_packages(materials))
    native_sources = []
    if native_cache is not None:
        source_lock = json.loads((ROOT / "scripts/sherpa/source-lock.json").read_text())
        for group in ("source_archives", "onnxruntime_cmake_dependencies"):
            for row in source_lock[group]:
                if row["id"] == "eigen":
                    path = native_cache / row["file_name"]
                    checked(path.read_bytes(), row)
                    native_sources.append(row)
    output.mkdir(parents=True)
    for name in ("LICENSE", "NOTICE", "BRAND.md", "THIRD_PARTY_NOTICES.md"):
        shutil.copyfile(ROOT / name, output / name)
    for name in ("MODEL-LICENSE.md", "VENDORED-SOURCES.md", "VENDORED-SOURCES.json", "materials.json"):
        shutil.copyfile(HERE / name, output / name)
    for row in materials["files"]:
        target = output / "upstream" / Path(row["path"]).name
        target.parent.mkdir(exist_ok=True)
        target.write_bytes((ROOT / row["path"]).read_bytes())
    inventory = []
    combined = ["SeaSnail dependency notices\n", (ROOT / "NOTICE").read_text(),
                "Scope: macOS arm64 Rust normal/build graph, frontend production graph and Swift helper.\n",
                "Separately bundled native runtimes/models have their own notices.\n"]
    for info, files, archive in packages:
        combined.append(f"\n{'=' * 72}\n{info['ecosystem']}: {info['name']}@{info['version']}\nLicense: {info['license']}\n")
        if info.get("authors"):
            combined.append("Authors: " + "; ".join(info["authors"]) + "\n")
        if info.get("author"):
            author = info["author"]
            combined.append("Author: " + (author if isinstance(author, str) else json.dumps(author, ensure_ascii=False)) + "\n")
        info["license_files"] = []
        for name, data in sorted(files.items()):
            target = output / "texts" / (digest(data) + ".txt")
            target.parent.mkdir(exist_ok=True)
            target.write_bytes(data)
            info["license_files"].append({"upstream_path": name,
                                          "path": target.relative_to(output).as_posix(), "sha256": digest(data)})
            combined.append(f"\n--- {name} ---\n{data.decode('utf-8', errors='replace')}\n")
        if "MPL-2.0" in info["license"]:
            if archive is None:
                raise ValueError(f"Corresponding source missing for {info['name']}")
            source = output / "corresponding-source" / f"{info['name']}-{info['version']}.crate"
            source.parent.mkdir(exist_ok=True)
            source.write_bytes(archive)
            info["corresponding_source"] = {"path": source.relative_to(output).as_posix(), "sha256": digest(archive), "modifications": "none"}
        inventory.append(info)
    for row in native_sources:
        target = output / "native-corresponding-source" / row["file_name"]
        target.parent.mkdir(exist_ok=True)
        target.write_bytes((native_cache / row["file_name"]).read_bytes())
    write_json(output / "native-corresponding-source.json", {
        "scope": "Unmodified Eigen inputs to Sherpa and ONNX Runtime; other native notices remain in the runtime manifest",
        "files": [{"path": "native-corresponding-source/" + row["file_name"],
                   "source": row["url"], "sha256": row["sha256"], "size_bytes": row["size_bytes"],
                   "modifications": "none"} for row in native_sources],
    })
    (output / "DEPENDENCY-NOTICES.txt").write_text("".join(combined))
    write_json(output / "dependency-inventory.json", {
        "schema_version": 1, "target": "aarch64-apple-darwin",
        "scope": "conservative Rust normal/build, frontend production and Swift helper dependencies; native runtimes separate",
        "lock_sha256": {str(path): digest((ROOT / path).read_bytes()) for path in (
            Path("Cargo.lock"), Path("apps/desktop/pnpm-lock.yaml"),
            Path("apps/desktop/native/post-paste-monitor/Package.resolved"))},
        "packages": inventory,
    })
    write_json(output / "files.json", {"files": [
        {"path": p.relative_to(output).as_posix(), "size_bytes": p.stat().st_size, "sha256": digest(p.read_bytes())}
        for p in sorted(output.rglob("*")) if p.is_file()
    ]})
    print(f"Collected notices for {len(inventory)} dependencies: {output}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--cargo-home", type=Path, default=Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")))
    parser.add_argument("--native-cache", type=Path, help="Verified Sherpa source cache; include both Eigen source archives")
    args = parser.parse_args()
    try:
        collect(args.output, args.cargo_home, args.native_cache)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
