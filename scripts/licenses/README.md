# License materials

The repository's Apache-2.0 license covers SeaSnail-owned code. It does not
relicense copied components, development skills, fonts, model weights, or
dependencies.

`materials.json` records the source, revision, SHA-256 and scope of the checked-in
upstream texts. These texts are preserved verbatim, including copyright contacts
and model-card links relative to their original upstream repository. Those
snapshot-relative links are provenance, not local SeaSnail navigation. The shadcn MIT text also
covers the imported skills in `.agents/skills/shadcn` and
`.agents/skills/migrate-radix-to-base`; their attribution is in
`VENDORED-SOURCES.md`. Application icons are governed by `BRAND.md` instead.

Generate the application notices with Python 3.11+ after a frozen frontend
install and Rust build:

```sh
python3 scripts/licenses/collect.py --output dist/license-materials
```

The collector is offline. It uses the locked macOS arm64 Cargo dependency tree
(including build dependencies), the installed frontend production dependency
tree and the Swift helper's resolved pins. It checks crate archive checksums and collects actual upstream license and
NOTICE files. Checked-in, revision-pinned supplements cover packages whose
published archive omits those files. Missing materials cause a failure rather
than a guessed license. Output contains no local home-directory paths.

The App packager runs the same collector and includes its output under
`Contents/Resources/licenses/application/`. Rust MPL-2.0 crate archives are
included as corresponding source. Sherpa packaging also includes both locked
Eigen source archives, from `--native-cache` (or `SEASNAIL_SHERPA_CACHE` for the
packager). FFmpeg's separate matching source package is included under
`Contents/Resources/licenses/ffmpeg/`; an older bundle must be rebuilt first.
This is a conservative notice inventory, not
a claim that every inventoried build dependency is linked into the App, nor a
complete SBOM for separately bundled native runtimes. Those retain their own
verified manifests and notices.

See `MODEL-LICENSE.md` for the distinction between SenseVoice runtime code and
weights. The model license is a custom upstream agreement, not Apache-2.0 or MIT.
The upstream conversion archive does not identify an exact source-weight
revision; the record explicitly preserves this limitation instead of inventing
one. Publication of an App still requires the release checks in
`doc/releasing.md`, including native corresponding source and model review.
