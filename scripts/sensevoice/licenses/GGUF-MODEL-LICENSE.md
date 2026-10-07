# GGUF model and VAD license declarations

The locked artifacts below declare `Apache-2.0` in their model cards.  These
records are fixed from the indicated Hugging Face revisions on 2026-08-24.

| artifact | locked revision | SHA-256 | declared license | source |
| --- | --- | --- | --- | --- |
| SenseVoiceSmall Q8 GGUF | `cebc2cdd171e895d783040dbd15f10f3a76f7151` | `4ae45c94422de949b387e2e0fb10d7e14e4c42c69db30c3444ecc7d4b844b7c5` | Apache-2.0 | https://huggingface.co/FunAudioLLM/SenseVoiceSmall-GGUF/blob/cebc2cdd171e895d783040dbd15f10f3a76f7151/README.md |
| SenseVoiceSmall F16 GGUF | `e2488d1981145c78ccbf7afa2ef2f607c5c422e2` | `2389039651f4574dbd674f1f1e296b8b1147b2e19a5fd9c2cd69e82669c78d8e` | Apache-2.0 | https://huggingface.co/FunAudioLLM/SenseVoiceSmall-GGUF/blob/e2488d1981145c78ccbf7afa2ef2f607c5c422e2/README.md |
| SenseVoiceSmall F32 GGUF | `669f1de748f932a95ee3c7023b4393808a9a071f` | `62bbbd6bc97bdb55a53957c768f9e7f38e6b818fb720ba46d6ff52d4cc200ff0` | Apache-2.0 | https://huggingface.co/FunAudioLLM/SenseVoiceSmall-GGUF/blob/669f1de748f932a95ee3c7023b4393808a9a071f/README.md |
| FSMN-VAD GGUF | `5644b91e991622131f287c034f2a0424ba5be928` | `1270f2559c495f4e7b6e739541151027d360761a3fda43fc147034f5719f5479` | Apache-2.0 | https://huggingface.co/FunAudioLLM/fsmn-vad-GGUF/blob/5644b91e991622131f287c034f2a0424ba5be928/README.md |

The model cards’ YAML metadata is the upstream license declaration. The
complete Apache License 2.0 text is included as `APACHE-2.0.txt`.

The four fixed model-card revisions were fetched again on 2026-10-07 and each
still declares `license: apache-2.0`. Verbatim snapshots and their SHA-256/size
records are retained under `scripts/licenses/upstream` and
`scripts/licenses/materials.json` and accompany newly packaged Apps.

These declarations apply to the specific GGUF distributions above. They must
not be generalized to the default Sherpa ONNX archive or FunASR toolkit weights.
The original SenseVoiceSmall model card links a custom model agreement; see
`scripts/licenses/MODEL-LICENSE.md` for the separate evidence and redistribution
limits. Reconcile any applicable original-weight and conversion terms before
publicly distributing legacy model binaries.
