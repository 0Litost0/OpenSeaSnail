# Audio fixture replacement — 2026-10-07

The three macOS system-voice WAVs were removed from the new source repository.
Business-flow tests now use a reproducible, project-generated triangle wave;
real speech tests use two Google FLEURS test utterances under CC BY 4.0.
The PoC reuses the new English fixture. Existing PoC evidence remains historical
and is not evidence of results on the replacement corpus.

See [license and attribution](../tests/api_regression/assets/AUDIO-LICENSE.md),
[provenance](../tests/api_regression/assets/audio-provenance.json) and
[quality decision](../tests/api_regression/docs/quality-approval.v3.md).
Source selection and mandatory key phrases were fixed before inference. No
numeric quality thresholds or performance budgets were relaxed. Corpus,
reference and quality-rule versions changed to avoid reusing old baselines.
Original selected WAVs were verified by SHA-256; full upstream archives were
not downloaded and locally checksum-verified. This limitation is explicit in
provenance; recorded archive identities come from upstream metadata.

## Validation

Validation ran on macOS arm64 using an isolated source-only Git snapshot, local
Sherpa model resources and the project's pinned FFmpeg. Temporary snapshot
commits identify test input only; the prepared public repository has no initial
commit, remote or push. The original development repository was not modified.

- Quick run `1df2257b-0858-47ff-9635-a8e82f63553c`: all 17 cases passed on the
  first attempt, with successful teardown and no flaky cases.
- Formal `SHERPA-001` run `c82f00d8-09ed-44b7-b5fc-cdcb7a906f25`: three complete
  transcriptions per language passed. Chinese CER was 0; English WER was
  0.095238 (9.52%). Limits remain 10% and 15%, respectively; mandatory phrases,
  restart/readback of all six records and sidecar cleanup passed.
- [Recorded observations](../tests/api_regression/docs/baseline-observations.fleurs-v1.json)
  contain relative asset identities, actual transcripts and scores, with local
  process paths and session identifiers omitted. Rescoring passed. The formal
  run preceded the final catalog/license-identity binding; corpus, scoring and
  product Rust code were unchanged. This is not a public-commit acceptance run.
- Byte-for-byte regeneration of all three fixtures from verified original
  sources passed. Asset checks verified hashes, formats, attribution materials,
  original references and six quality rejection controls.
- Four integrity regression tests passed: missing license, changed procedural
  audio, edited reference and mismatched corpus identity are rejected.
- Final TypeScript and contract checks passed: 23 catalog cases, 22 required,
  17 quick; 7 valid examples accepted, 13 invalid examples and 4 catalog
  mutations rejected.

## Source review

The final source-only export contained 630 nonignored candidate files. All
project-owned Markdown file links resolved; verbatim upstream cards were
excluded from local-link checks. No owner home path or private email was found
in project-owned source. Temporary-directory matches were generic script
defaults, not personal directories. Gitleaks reported only the two existing
synthetic logging-redaction test literals; these were retained as test coverage.
All three old WAV paths were absent from the new repository. The original
repository remained clean at its previous commit, and its original WAV bytes
matched that commit. These scans support this source review; they do not certify
ignored build outputs or future additions for publication.

## Reproduction checks

From the repository root:

```sh
python3 tests/api_regression/scripts/check_assets.py
python3 tests/api_regression/scripts/test_audio_assets.py
python3 tests/api_regression/scripts/summarize_baseline.py
```

Regeneration additionally requires the verified source WAVs and FFmpeg; follow
[AUDIO-LICENSE.md](../tests/api_regression/assets/AUDIO-LICENSE.md).
Raw local run reports and build resources stay ignored. This replacement did
not include full workspace tests, all 22 required API cases, a new App bundle,
macOS browser/UI acceptance or signing checks. Model-binary provenance remains
a separate item in the release checklist.
