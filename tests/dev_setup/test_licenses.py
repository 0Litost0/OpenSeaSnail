"""License collection must reject incomplete or unverifiable release materials."""
import hashlib
from contextlib import redirect_stderr
import importlib.util
import io
import json
from pathlib import Path
import re
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


collector = load("license_collector", ROOT / "scripts/licenses/collect.py")
ffmpeg = load("ffmpeg_source", ROOT / "scripts/licenses/verify-ffmpeg-source.py")
doctor = load("license_doctor", ROOT / "scripts/doctor.py")


class LicenseCollectionTests(unittest.TestCase):
    def test_old_python_is_reported_before_attempting_an_app_build(self):
        errors = io.StringIO()
        with patch.object(doctor.sys, "argv", ["doctor.py"]), \
                patch.object(doctor.sys, "version_info", (3, 10, 0)), \
                patch.object(doctor.shutil, "which", return_value=None), redirect_stderr(errors):
            self.assertEqual(doctor.main(), 1)
        self.assertIn("Python 3.11 or newer", errors.getvalue())

    def make_crate(self, root, with_license):
        archive = root / "cargo/registry/cache/example/fixture-1.0.0.crate"
        archive.parent.mkdir(parents=True)
        files = {"Cargo.toml": b'[package]\nname="fixture"\nversion="1.0.0"\nlicense="MIT"\n'}
        if with_license:
            files["LICENSE"] = b"Fixture upstream license text"
        with tarfile.open(archive, "w:gz") as tar:
            for name, data in files.items():
                member = tarfile.TarInfo("fixture-1.0.0/" + name)
                member.size = len(data)
                tar.addfile(member, io.BytesIO(data))
        checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
        (root / "Cargo.lock").write_text(
            'version=4\n[[package]]\nname="fixture"\nversion="1.0.0"\n'
            'source="registry+https://example.com/index"\nchecksum="' + checksum + '"\n')
        return archive

    def test_package_metadata_does_not_substitute_for_missing_license_text(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.make_crate(root, with_license=False)
            with patch.object(collector, "ROOT", root), patch.object(
                    collector.subprocess, "check_output", return_value="fixture v1.0.0\n"):
                with self.assertRaisesRegex(ValueError, "Missing license text"):
                    list(collector.rust_packages({"dependency_supplements": []}, root / "cargo"))

    def test_changed_crate_is_not_accepted_even_with_a_license(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = self.make_crate(root, with_license=True)
            archive.write_bytes(archive.read_bytes() + b"tampered")
            with patch.object(collector, "ROOT", root), patch.object(
                    collector.subprocess, "check_output", return_value="fixture v1.0.0\n"):
                with self.assertRaisesRegex(ValueError, "checksum-verified crate"):
                    list(collector.rust_packages({"dependency_supplements": []}, root / "cargo"))

    def test_self_consistent_ffmpeg_manifest_cannot_override_reviewed_source_pin(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "source-manifest.json").write_text(json.dumps({"archive_sha256": "0" * 64}))
            with self.assertRaisesRegex(ValueError, "locked archive"):
                ffmpeg.verify(root)

    def test_ffmpeg_source_manifest_must_match_the_distributed_executable(self):
        source_pin = re.search(r'^SHA256="([0-9a-f]{64})"$',
                               (ROOT / "scripts/ffmpeg/build-macos-arm64.sh").read_text(), re.M).group(1)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "ffmpeg").write_bytes(b"different executable")
            (root / "source-manifest.json").write_text(json.dumps({
                "archive_sha256": source_pin, "binary_sha256": "0" * 64,
            }))
            with self.assertRaisesRegex(ValueError, "binary does not match"):
                ffmpeg.verify(root)


if __name__ == "__main__":
    unittest.main()
