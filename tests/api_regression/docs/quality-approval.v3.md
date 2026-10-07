# Licensed fixture replacement — quality-v3

On 2026-10-07 the maintainer requested replacement of the three macOS
system-voice WAVs to remove their public-distribution license uncertainty.
This record applies that authorization to a new dataset; it is not a new
approval of model redistribution or a general quality claim.

- Business tests now use procedural audio; real ASR retains Chinese and English
  speech from the official CC-BY-4.0 FLEURS test set.
- Dataset/reference/rules are independently versioned as `fleurs-test-v1`,
  `reference-fleurs-v1` and `quality-v3`.
- The prior Chinese CER <= 10%, English WER <= 15% and normalization are retained.
  No numerical threshold was raised. All new reference key phrases are required;
  the previous corpus's optional English brand rule does not carry over.
- Samples were selected before inference as the first eligible archive members:
  Chinese 20-40 characters without Latin/digits/parentheses; English 60-150
  characters without digits/hyphens. Full utterances are preserved. Source texts
  are unchanged, and key phrases were set before observing ASR output.
- The old system-voice observations are not a baseline for the new samples.
  New acceptance must run three repetitions per language and verify persistence
  and process cleanup. Any failure stays a failure; no tuning to observed output.
- Existing timing/evidence budgets are unchanged. Audio licensing and this
  regression decision are independent of the default ONNX weight provenance gap.

See [audio attribution](../assets/AUDIO-LICENSE.md) and
[coverage design](design.md). Future data/reference/scorer/threshold changes
require another recorded review and new identity.
