# SeaSnail bundled ffmpeg (macOS arm64)

SeaSnail uses `ffmpeg` to normalize uploaded/imported audio to 16 kHz, mono,
signed 16-bit WAV before local ASR. Realtime recording is already captured as
16 kHz mono WAV and does not invoke `ffmpeg`. The executable is built from the
official FFmpeg GitHub `n9.0.1` tag source with a pinned SHA-256 by
[`scripts/ffmpeg/build-macos-arm64.sh`](../../../scripts/ffmpeg/build-macos-arm64.sh).

The build disables all external libraries and GPL options, and links FFmpeg's
own libraries statically. The resulting executable needs only macOS system
frameworks at runtime; it never uses Homebrew or another package manager.

Build it with:

```bash
scripts/ffmpeg/build-macos-arm64.sh
```

The generated `bundle/` and source `cache/` are intentionally ignored by Git.
The bundle includes FFmpeg's LGPL-2.1-or-later text, the exact source URL,
SHA-256 and configure summary in `BUILD-INFO.txt`. It also contains the exact
source archive, actual build configuration, build script and a no-modifications
record under `corresponding-source/`. `source-manifest.json` binds those files
to the executable's SHA-256. The App packager verifies and includes this source
package under `Contents/Resources/licenses/ffmpeg/`, so recipients receive the
corresponding source rather than only an upstream URL. An old bundle without
these materials must be rebuilt before packaging.
