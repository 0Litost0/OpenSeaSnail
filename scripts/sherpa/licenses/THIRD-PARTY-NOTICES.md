# Sherpa ONNX candidate third-party notices

This inventory is a packaging input, not a substitute for the complete license
texts. The final artifact must include every item below.

| Component | Locked identity | License material |
| --- | --- | --- |
| sherpa-onnx | `1cb484af5e69d3c7803c1eb0b3b5ab8041e0e911` (`v1.13.6`) | Apache-2.0; complete `LICENSE` from the verified checkout |
| kaldi-native-fbank | `v1.22.3` archive in `source-lock.json` | Apache-2.0; archive `LICENSE` |
| kaldi-decoder | `v0.3.0` archive in `source-lock.json` | Apache-2.0; archive `LICENSE` |
| simple-sentencepiece | `v0.7` archive in `source-lock.json` | Apache-2.0; archive `LICENSE` |
| nlohmann/json | `v3.12.0` archive in `source-lock.json` | `NLOHMANN-JSON-MIT.txt` |
| kaldifst | `v1.8.0` archive in `source-lock.json` | Apache-2.0; archive `LICENSE` |
| OpenFST | `v1.8.5-2026-07-09` archive in `source-lock.json` | Apache-2.0; archive `COPYING` |
| Eigen | `v5.0.1`, compiled with `EIGEN_MPL2_ONLY` | archive `COPYING.README`, `COPYING.MPL2`, and `COPYING.BSD` |
| kissfft | revision `febd4caeed32e33ad8b2e0bb5ea77542c40f18ec` | archive `COPYING` and `LICENSES/BSD-3-Clause` |
| ONNX Runtime | `v1.27.1`, `df2ba1cf8108aa63627cf4cdf8f807880b938616`, built from the verified checkout | `ONNXRUNTIME-MIT.txt` and the checkout's verified `ThirdPartyNotices.txt` |
| SenseVoice int8 model | GitHub release asset `288366523`; official SenseVoiceSmall model and Sherpa conversion | Custom FunASR model agreement; see `scripts/licenses/MODEL-LICENSE.md` and the pinned evidence in `scripts/licenses/materials.json` |
| Silero VAD v4 export | GitHub release asset `271935959`; upstream commit `915dd3d639b8333a52e001af095f87c5b7f1e0ac` | `SILERO-VAD-MIT.txt` |

Do not copy a license or notice from an unverified system installation. The
later artifact preparation task must extract archive-owned notices only after
the enclosing archive passes the lock-file hash and size checks. ONNX Runtime's
notice must come from its verified source checkout.

`FUNASR-MIT.txt` and the archive's 71-byte license pointer describe toolkit
code; they must not be presented as the model weights' license. The App packager
adds the complete model agreement, attribution and historical evidence under
`Contents/Resources/licenses/application/`. The verified runtime artifact's
original license pointer remains unchanged for provenance. Its existing hashes
do not constitute approval to redistribute the weights.

The GGUF Apache-2.0 text in `scripts/sensevoice/licenses/APACHE-2.0.txt` is now
the complete standard text. Model-card declarations still require their own
revision-specific review; they do not override the default ONNX model terms.
