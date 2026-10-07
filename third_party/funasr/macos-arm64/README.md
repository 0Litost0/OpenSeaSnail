# FunASR macOS arm64 dependency cache

This directory is the stable, repository-local location for inputs and output
of the SeaSnail self-contained FunASR runtime:

```text
third_party/funasr/macos-arm64/
├─ cache/       # downloaded standalone Python and PyPI wheels
└─ bundle-process-name/ # generated runtime, later copied into SeaSnail.app Resources
```

`bundle-process-name/` is the canonical release input. During development a separately named
complete bundle may be validated without moving it by passing
`scripts/macos/build-dev-app.sh --funasr-root /absolute/path/to/bundle`.
This override is only for the unsigned development app; release assembly must
first rebuild/verify the canonical `bundle-process-name/` and its manifests.

The binary contents are intentionally ignored by ordinary Git: Python, Torch,
and model weights are too large for source control. The canonical versions,
hashes, and licenses live in `scripts/funasr/`; a release-grade copy of these
assets must be published through Git LFS or an artifact registry if it needs to
be shared without re-downloading on another machine.

Browser downloads on macOS may add Gatekeeper's `com.apple.quarantine`
attribute to the standalone Python archive and its extracted interpreter. The
download/build scripts verify the pinned archive SHA-256 first and then remove
that attribute only from the verified, repository-local interpreter. This is a
development-build convenience, not the end-user distribution mechanism: the
final `.app` must codesign all nested executable code and dylibs, then be
notarized and stapled.

From zero (empty `cache/`), prepare the bundle in order:

```bash
# 1. Fetch the pinned standalone Python archive, download wheels, and prepare a
#    bootstrap interpreter with modelscope installed. (network: PyPI wheels)
scripts/funasr/download-wheels-macos-arm64.sh

# 2. Download the four default offline models into cache/models/ using the
#    bootstrap interpreter from step 1. (network: ModelScope)
third_party/funasr/macos-arm64/cache/python-bootstrap/python/bin/python3 \
  scripts/funasr/download-models-macos-arm64.py

# 3. Assemble the bundle offline from cache/. Uses the committed requirements.lock.
#    By default all four models (asr/vad/punc/spk) are bundled. Pass --slim to
#    ship only asr+vad: punc/spk are omitted from the bundle but their entries
#    remain in models-manifest.json, so the runtime downloads them on demand
#    from ModelScope (SHA-256 verified, failure degrades to native punctuation
#    without blocking transcription). --slim saves ~1.1 GB.
scripts/funasr/build-macos-arm64.sh
```

`scripts/funasr/prepare-bundle.sh` runs all three steps in order (forwarding
extra args like `--output DIR` or `--slim` to step 3); it is a dev convenience
and is idempotent/resumable.

The model downloader (`download-models-macos-arm64.py`) resumes an interrupted
component but leaves completed components untouched. It writes
`models-manifest.json` with the ID, requested revision and SHA-256 of every
model file, and rejects any `*.incomplete` weight file. Review the upstream
model licenses and retain their notices before shipping a release. It serializes
downloads and retries interrupted transfers six times with backoff; rerunning it
later is also safe and resumes the incomplete component.

`download-wheels-macos-arm64.sh` uses `pip wheel`, rather than `pip download
--only-binary`, because a small number of required pure-Python dependencies (for
example `jieba`) may be published only as source archives. It builds them into
local wheels and then installs the full dependency set into the bootstrap
interpreter, so the later bundle build and the model download stay offline.

`build-macos-arm64.sh --bootstrap-lock` is the maintainer path to regenerate the
dependency lock: it resolves `requirements.in` once and writes
`cache/requirements.lock.candidate`, which must be reviewed and committed as
`scripts/funasr/requirements.lock` before a release build can be called
reproducible. A normal build uses the committed lock directly (no
`--bootstrap-lock`).
