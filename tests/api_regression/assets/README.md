# Versioned regression assets

Business tests and real speech recognition use different inputs:

- `pipeline-audio.json` pins a one-second procedural triangle-wave WAV. The
  deterministic host supplies fixed text; no speech is needed for upload,
  normalization, persistence, cleanup-provider or retry tests.
- `manifest.json` pins Chinese and English FLEURS test utterances, unchanged
  upstream references and mandatory key phrases. These licensed read-speech
  recordings are for real Sherpa integration checks, not a general benchmark.

The old three macOS system-voice WAVs have been removed. See
[AUDIO-LICENSE.md](AUDIO-LICENSE.md) for CC BY 4.0 attribution, exact source
revision/member/hash records, conversion details and reproduction instructions.
The procedural waveform is Apache-2.0; FLEURS speech retains CC BY 4.0.

```sh
python3 tests/api_regression/scripts/check_assets.py
python3 tests/api_regression/scripts/test_audio_assets.py
```

Checks verify the procedural bytes, speech/reference hashes, WAV formats,
source-record/transcript identity, full license evidence and negative controls.
All execution uses committed WAVs; no TTS service or downloads are required.

## Quality rules

`quality-rules.v3.json` targets `fleurs-test-v1` / `reference-fleurs-v1`.
Chinese CER <= 10% and English WER <= 15% retain the previous numeric limits
and normalization. All new key phrases are mandatory; there is no optional
brand-word rule in this dataset. See [replacement decision](../docs/quality-approval.v3.md).
`quality-negatives.v3.json` pins six rejection controls, while checks additionally
remove every key phrase and verify whole-word English matching.

```sh
python3 tests/api_regression/scripts/quality.py --sample fleurs-en-v1 --text-file /path/to/transcript.txt
```

`scenarios/asr.json` and `scenarios/provider.json` define deterministic behavior
for the test host, without replacing production authentication, storage or
pipeline composition. Live provider endpoints/credentials remain in environment
variables or ignored local configuration, not fixture files.

## Baseline and measurements

The previous system-voice baseline is not applicable to these new recordings.
A new formal `SHERPA-001` run must transcribe each full utterance three times,
check content, restart and read all six persisted results, then confirm sidecar
cleanup. The new recorded observations are in
`../docs/baseline-observations.fleurs-v1.json`; re-score with:

```sh
python3 tests/api_regression/scripts/summarize_baseline.py
```

Analysis output stays under ignored `artifacts/baseline/`. New PoC observations
are also ignored and remain separate from formal-host acceptance. A change in
audio, references, scoring, thresholds or budgets requires reviewed new identities;
regeneration must not silently update hashes or weaken limits to hide a failure.
