"""Behavioral tests for safe first-run preparation (no network or user-data access)."""
import hashlib
import importlib.util
import platform
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


doctor = load("doctor", ROOT / "scripts/doctor.py")
fetch = load("fetch", ROOT / "scripts/sherpa/fetch-inputs.py")
reproducible = load("reproducible", ROOT / "scripts/sherpa/reproducible-build.py")


class SetupTests(unittest.TestCase):
    @unittest.skipUnless(platform.system() == "Darwin", "Native RPATH regression uses Mach-O")
    def test_link_rpaths_do_not_make_shared_library_directory_dependent(self):
        with tempfile.TemporaryDirectory() as directory:
            binaries = []
            for name in ("short", "a much longer checkout directory"):
                root = Path(directory).resolve() / name
                (root / "vendor").mkdir(parents=True)
                (root / "dep.c").write_text("int dep(void) { return 42; }\n")
                subprocess.run(["cc", "-dynamiclib", "-Wl,-install_name,@rpath/libdep.dylib",
                                str(root / "dep.c"), "-o", str(root / "vendor/libdep.dylib")], check=True)
                (root / "fixture.c").write_text('extern int dep(void);\n'
                                              'const char *source_path(void) { return __FILE__; }\n'
                                              'int value(void) { return dep(); }\n')
                (root / "CMakeLists.txt").write_text('cmake_minimum_required(VERSION 3.24)\n'
                                                   'project(Fixture LANGUAGES C)\n'
                                                   'set(CMAKE_INSTALL_RPATH_USE_LINK_PATH TRUE)\n'
                                                   'add_library(sherpa-onnx-c-api SHARED fixture.c)\n'
                                                   'target_link_libraries(sherpa-onnx-c-api PRIVATE "${CMAKE_SOURCE_DIR}/vendor/libdep.dylib")\n')
                flags = shlex.join(reproducible.prefix_flags(root, root / "sherpa", root / "build", root / "scripts"))
                hook = ROOT / "scripts/sherpa/reproducible-rpath.cmake"
                for command in (["cmake", "-S", str(root), "-B", str(root / "build"),
                                 "-DCMAKE_BUILD_TYPE=Release", f"-DCMAKE_C_FLAGS={flags}",
                                 f"-DCMAKE_PROJECT_INCLUDE={hook}"],
                                ["cmake", "--build", str(root / "build")]):
                    result = subprocess.run(command, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                binary = root / "build/libsherpa-onnx-c-api.dylib"
                commands = subprocess.check_output(["otool", "-l", str(binary)], text=True).splitlines()
                rpaths = [commands[index + 2].split()[1] for index, line in enumerate(commands)
                          if line.strip() == "cmd LC_RPATH"]
                self.assertEqual(rpaths, ["@loader_path"])
                binaries.append(binary.read_bytes())
            self.assertEqual(*binaries)

    @unittest.skipUnless(platform.system() == "Darwin", "Native reproducibility fixture uses Apple clang")
    def test_native_file_paths_do_not_change_compiled_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            binaries = []
            # Include spaces and a substantially different prefix length.
            for name in ("short", "a much longer checkout directory"):
                root = Path(directory).resolve() / name
                root.mkdir()
                source = root / "fixture.c"
                source.write_text('#include <stdio.h>\nint main(void) { puts(__FILE__); }\n')
                binary = root / "fixture"
                flags = reproducible.prefix_flags(root, root / "sherpa", root / "build", root / "scripts")
                subprocess.run(["cc", "-O2", *flags, str(source), "-o", str(binary)], check=True)
                self.assertEqual(subprocess.check_output([str(binary)], text=True).strip(),
                                 "/seasnail/onnxruntime/fixture.c")
                binaries.append(binary.read_bytes())
            self.assertEqual(*binaries)

    def test_unreviewed_native_toolchain_is_rejected(self):
        with patch.object(reproducible.subprocess, "check_output", return_value="unexpected-version\n"):
            with self.assertRaisesRegex(ValueError, "Native artifact toolchain mismatch"):
                reproducible.check_toolchain()

    def test_node_boundaries_match_supported_toolchain(self):
        for version in ("22.22.2", "22.23.0", "24.15.0", "26.0.0", "27.1.0"):
            self.assertTrue(doctor.supported_node(version), version)
        for version in ("22.12.0", "22.22.1", "23.9.0", "24.14.9", "25.9.0", "20.0.0", "unknown"):
            self.assertFalse(doctor.supported_node(version), version)

    def row(self, data=b"locked bytes"):
        return {"file_name": "model.bin", "url": "https://example.com/model.bin", "size_bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}

    def test_mismatched_existing_file_is_not_overwritten_or_downloaded(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            target = cache / "model.bin"
            target.write_bytes(b"user data")
            with patch.object(fetch.subprocess, "run") as network:
                with self.assertRaises(ValueError):
                    fetch.download(cache, self.row())
                network.assert_not_called()
            self.assertEqual(target.read_bytes(), b"user data")

    def test_matching_cache_is_reused_without_network(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            (cache / "model.bin").write_bytes(b"locked bytes")
            with patch.object(fetch.subprocess, "run") as network:
                fetch.download(cache, self.row())
                network.assert_not_called()

    def test_invalid_download_never_becomes_a_cached_input(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            def bad_download(arguments, **_kwargs):
                Path(arguments[arguments.index("--output") + 1]).write_bytes(b"wrong bytes!")
            with patch.object(fetch.subprocess, "run", side_effect=bad_download):
                with self.assertRaises(ValueError):
                    fetch.download(cache, self.row())
            self.assertEqual(list(cache.iterdir()), [])

    def test_symlink_and_path_escape_are_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            (cache / "original").write_bytes(b"locked bytes")
            (cache / "model.bin").symlink_to(cache / "original")
            with self.assertRaises(ValueError):
                fetch.download(cache, self.row())
            row = self.row()
            row["file_name"] = "../escape"
            with self.assertRaises(ValueError):
                fetch.download(cache, row)
            self.assertEqual((cache / "original").read_bytes(), b"locked bytes")

    def test_source_checkout_rejects_non_https_and_unpinned_revisions(self):
        with tempfile.TemporaryDirectory() as directory:
            sources = Path(directory)
            for row in ({"repository": "file:///untrusted", "revision": "a" * 40},
                        {"repository": "https://example.com/repo.git", "revision": "--unsafe-option"}):
                with patch.object(fetch.subprocess, "run") as command:
                    with self.assertRaises(ValueError):
                        fetch.checkout(sources, "source", row)
                    command.assert_not_called()
            self.assertEqual(list(sources.iterdir()), [])

    def test_valid_download_is_published_only_after_verification(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            def valid_download(arguments, **_kwargs):
                self.assertIn("--proto-redir", arguments)
                self.assertNotIn("--insecure", arguments)
                Path(arguments[arguments.index("--output") + 1]).write_bytes(b"locked bytes")
            with patch.object(fetch.subprocess, "run", side_effect=valid_download):
                fetch.download(cache, self.row())
            self.assertEqual((cache / "model.bin").read_bytes(), b"locked bytes")
            self.assertEqual(len(list(cache.iterdir())), 1)


if __name__ == "__main__":
    unittest.main()
