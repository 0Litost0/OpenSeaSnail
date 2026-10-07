# Sherpa ONNX macOS arm64 build inputs

`source-lock.json` and `artifact-lock.json` are the source of truth for the
first Sherpa ONNX candidate. URLs are acquisition hints only; SHA-256 and exact
byte size are authoritative. A release build must use pre-fetched local inputs
and must not allow CMake or the preparation step to fall back to the network.

## Locked source and target

- Sherpa ONNX: `v1.13.6`, revision
  `1cb484af5e69d3c7803c1eb0b3b5ab8041e0e911`.
- Target: macOS arm64 (`arm64-apple-darwin`), shared Sherpa/ONNX Runtime native
  libraries, C API enabled.
- ONNX Runtime: official source tag `v1.27.1` at revision
  `df2ba1cf8108aa63627cf4cdf8f807880b938616`, including its three pinned Git
  submodules and the 14 FetchContent archives selected by the CPU-only macOS
  arm64 build. The third-party prebuilt dylib is intentionally not used: it
  declares `minos=26.4`.
- Unconditional minimal-build FetchContent inputs: kaldi-native-fbank 1.22.3,
  kaldi-decoder 0.3.0, simple-sentencepiece 0.7, and nlohmann/json 3.12.0.
  Their active transitive closure is also locked: kissfft, kaldifst, Eigen 5.0.1,
  and OpenFST. Test-only googletest and Python-only pybind11 remain disabled and
  are intentionally absent.
- Sherpa prepends its own CMake module directory. Consequently the nested
  `include(eigen)` and `include(openfst)` calls resolve to the pinned top-level
  Sherpa modules (Eigen 5.0.1 and OpenFST 1.8.5-2026-07-09), not the older
  versions named inside the kaldi-decoder/kaldifst archives.
- Compile with `EIGEN_MPL2_ONLY` so LGPL-only Eigen headers fail at compile time;
  the distributable Eigen license set is MPL-2.0/BSD-3-Clause.
- Disabled surfaces: demo binaries and C API examples, Python, tests,
  PortAudio, WebSocket, TTS, and speaker diarization.
- CoreML is disabled in both ONNX Runtime and Sherpa for this CPU-only
  SenseVoice candidate (`SHERPA_ONNX_DISABLE_COREML`), avoiding optional
  provider symbols and deployment-target coupling.
- ONNX Runtime and Sherpa are both built with
  `CMAKE_OSX_DEPLOYMENT_TARGET=13.0`; every installed Mach-O is checked after
  build and a higher deployment target is rejected.
- Local patches are not permitted. A future patch must be stored in the repo,
  listed with its SHA-256, and covered by a contract regression before use.

The reviewed native artifact toolchain is fixed in `native-toolchain.json`:
CMake 4.4.2, Apple clang 21.0.0 (`clang-2100.1.1.101`), and macOS SDK 26.5
(build `25F70`). `doctor.py` and the native build entry reject a mismatch before
compilation. Select the appropriate Xcode/Command Line Tools and CMake version;
unset custom `CC`, `CXX`, `CFLAGS`, `CXXFLAGS`, and `LDFLAGS`. UI/Rust-only
development does not need this native artifact toolchain.

Release builds also normalize paths with `-ffile-prefix-map`: ONNX Runtime
source uses `/seasnail/onnxruntime`, Sherpa source `/seasnail/sherpa-onnx`,
native build/dependency files `/seasnail/build`, and project-native sources
`/seasnail/scripts`. Debug compilation directories use `/seasnail`. This is
needed even with Release optimization: upstream `__FILE__` values are embedded
in runtime error strings. The earlier artifact recorded temporary checkout
paths, so rebuilding the same sources elsewhere changed sizes and hashes.
There are no upstream source patches and no relaxation of artifact validation.

`reproducible-rpath.cmake` disables automatic installation link-path RPATHs on
the final Sherpa shared targets before CMake generates the linker command.
They link with only `@loader_path`. Deleting absolute RPATHs after linking is
insufficient: the path length has already affected Mach-O section offsets and
the link UUID. The build rejects unexpected RPATHs instead of masking them.

Changing this toolchain or build configuration is a reviewed artifact change:
build in independent directories, compare native bytes, retain the old
baseline, and run the native smoke and full API acceptance before updating the
canonical manifest/catalog and versioned test assets. A mismatch by itself is
not authorization to regenerate a lock.

## Locked runtime artifacts

- SenseVoice int8 release archive, GitHub asset `288366523`.
- `model.int8.onnx` and `tokens.txt`, verified both through the archive digest
  and through individual extracted-entry digests.
- Sherpa's Silero VAD v4 export, GitHub asset `271935959`.

The production artifact size in `artifact-lock.json` counts only the locked model
archive inputs. The prepared App artifact is defined by
`artifact-manifest.macos-arm64.json`; its catalog size is the exact sum of the
installed model, tokens, VAD, sidecar, native libraries, and any manifest-listed
runtime files. `prepare-macos-arm64.sh` copies the canonical manifest and rejects
any extracted file whose size or SHA-256 differs from it.

## Offline verification

Place every `file_name` from both lock files into one cache directory and keep
clean checkouts of both source repositories, then run:

```sh
scripts/sherpa/verify-locks.sh \
  --cache /path/to/cache \
  --source /path/to/clean/sherpa-onnx \
  --onnxruntime-source /path/to/clean/onnxruntime
```

The verifier rejects a missing file, size/hash drift, a wrong or dirty Sherpa
checkout, ONNX Runtime/submodule/dependency-manifest drift, or drift in the
required extracted model entries. The build constructs ONNX Runtime's URL-shaped
dependency mirror exclusively from verified cache files and does not access the
network.

## License closure

The source and artifact locks record the exact license source, revision, hash,
and/or archive path. [licenses/THIRD-PARTY-NOTICES.md](licenses/THIRD-PARTY-NOTICES.md)
is the packaging checklist. Apache-2.0's complete text must come from the
verified Sherpa checkout and verified dependency archives; it must not use the
older abbreviated GGUF copy. The packaging step must copy it together with the
component-specific notices and all MIT texts. ONNX Runtime's complete
`ThirdPartyNotices.txt` must come from the verified ONNX Runtime checkout, not
from an unpinned local installation.
