# SenseVoice GGUF server build inputs

`source-lock.json` is the source of truth for the upstream revision used to build
the bundled `sensevoice-server`. The build script introduced in ST-M1.3 must
read this file and reject a source checkout at a different revision.

## Locked source

- Repository: `https://github.com/QwenAudio/SenseVoice.git`
- Revision: `6991744856587fa44379e8b5dcc432debffeb1be`
- Runtime source: `runtime/llama.cpp`
- Build target: `sensevoice-server`

The server sources are vendored in the SenseVoice checkout and are locked by
the SenseVoice revision. They are not a Git submodule. However, their CMake
configuration uses `FetchContent` to obtain a separate `llama.cpp` dependency:

- Repository: `https://github.com/ggml-org/llama.cpp.git`
- Revision: `8086439a4cea94c71a5dfb8fe4ad1546aebd640f`

The build script introduced in ST-M1.3 must receive a pre-fetched, clean local
checkout of this exact `llama.cpp` revision, verify it, and pass it as
`-DFETCHCONTENT_SOURCE_DIR_LLAMA=<checkout>`. It must not let CMake fetch the
dependency or otherwise access the network during configuration or build.

## Patch policy

No local source patches are permitted for the first server PoC. Any future
patch must be committed under `scripts/sensevoice/patches/`, listed in
`source-lock.json` with its SHA-256, and covered by a build/contract regression
test before it can be used in a candidate App.

## Toolchain contract

The pinned source declares CMake 3.16+ and C++17. ST-M1.3 will use a Release
CMake build for macOS arm64. Apple Command Line Tools or full Xcode provides
the compiler; Python, pip, package managers, model download tools, and network
fallbacks are not build dependencies.

The developer machine used to create this lock reported CMake 4.4.2 and Apple
clang 21.0.0. These are observations, not exact toolchain locks: the minimum
compatibility contract remains CMake 3.16+ with an Apple C++17 compiler.
