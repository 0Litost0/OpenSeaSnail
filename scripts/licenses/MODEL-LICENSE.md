# SenseVoiceSmall model attribution and license evidence

Model authors/source: Alibaba Group / FunAudioLLM / FunASR,
[SenseVoiceSmall](https://huggingface.co/FunAudioLLM/SenseVoiceSmall) and
[`iic/SenseVoiceSmall`](https://modelscope.cn/models/iic/SenseVoiceSmall).
SeaSnail does not train or claim ownership of these weights.

## Default Sherpa ONNX int8 distribution

SeaSnail downloads the k2-fsa/sherpa-onnx conversion archive identified by
`scripts/sherpa/artifact-lock.json`: release asset `288366523`, filename
`sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2`, SHA-256
`7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e`.
The extracted int8 weights have SHA-256
`c71f0ce00bec95b07744e116345e33d8cbbe08cef896382cf907bf4b51a2cd51`.
The ONNX conversion/quantization is supplied by the Sherpa project; SeaSnail
redistributes the locked bytes without modifying the weights or tokens.

The archive's 71-byte `LICENSE` points to the FunASR **toolkit's MIT license**.
That pointer is retained for provenance, but it is not sufficient evidence that
model weights are MIT licensed. The toolkit and model licenses are separate.

The official model card at Hugging Face revision
`3847d57b6bdf2dd8875cb1508d2af43d80a16bf7` declares `license: other` and links to
FunASR's `MODEL_LICENSE`. Its verbatim snapshot is
`upstream/SENSEVOICE-SMALL-MODEL-CARD.md`. The corresponding agreement is preserved
as `upstream/FUNASR-MODEL-1.1.txt`, from FunASR commit
`58830eca4012644aac0c3218c3ccc7d98f003fda`. The local identifier
`LicenseRef-FunASR-Model-1.1` identifies this custom agreement; it is not an SPDX
standard license or an OSI-approval claim.

The agreement permits use, copying, modification and sharing subject to its
terms, requires source/author attribution and retention of model names, and
contains additional conduct, termination and revision provisions. Distribute
the full agreement and this attribution with the weights. Do not describe the
weights as covered by SeaSnail's Apache-2.0 license. Do not infer additional
permissions by summarizing the agreement; its actual text controls.

## Historical evidence and limits

The 2024-07-17 official HF snapshot
`72963f9eb91c0ced005b957655ca709d863532ea` has no license YAML header or separate
LICENSE in its file tree. Its model card is preserved as
`upstream/SENSEVOICE-SMALL-MODEL-CARD-2024.md`. The pinned HF file tree is retained
as `upstream/SENSEVOICE-SMALL-FILES-2024.json`, including the original `model.pt`
LFS SHA-256; it does not establish which snapshot the converter used.
FunASR's then-current model
agreement, version 1.0 at `75ddde7acd49cb6940de339cf7de24297f0826c2`, is retained
as historical evidence, **not** as a replacement for version 1.1 or a claim that
the conversion archive is grandfathered under version 1.0.

The Sherpa export code at locked runtime revision
`1cb484af5e69d3c7803c1eb0b3b5ab8041e0e911`,
`scripts/sense-voice/export-onnx.py`, loads `iic/SenseVoiceSmall` without a source
revision. The conversion archive does not provide the original weight hash or
export revision. Matching its date to an HF commit would not establish the
conversion's exact provenance. Before public weight redistribution, obtain
that linkage from the distributor or reproduce the conversion from an explicitly
licensed, pinned source snapshot and review the applicable agreement. Source
publication without model binaries does not distribute these weights.

## Legacy runtimes

FunASR toolkit MIT, SenseVoice runtime MIT, and llama.cpp MIT apply to their
respective **code**, not automatically to the weights. The locked GGUF model
cards declare Apache-2.0; preserve those exact revision-specific declarations,
but do not use them to relabel the default ONNX weights. Differences between
converted-model declarations and the original model terms must be resolved
before distributing the legacy model binaries as well.
