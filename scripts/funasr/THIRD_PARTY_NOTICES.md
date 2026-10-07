# FunASR runtime third-party notices

This directory describes the notice obligations for the default macOS arm64
FunASR bundle. The release process must copy the applicable license texts into
the final `.app/Contents/Resources/funasr/licenses/` directory and record the
exact artifact hashes in `bundle-manifest.json`.

| Component | Intended version | License / source of truth | Release action |
|---|---:|---|---|
| CPython standalone | 3.11.16 / 20260814 | CPython PSF License | Preserve its bundled license text. |
| FunASR toolkit | 1.4.2 | MIT | Preserve its wheel `dist-info/licenses/LICENSE` and attribution. |
| PyTorch / torchaudio | 2.11.0 | BSD-style (upstream distribution) | Preserve wheel license/NOTICE files. |
| ModelScope client | resolved lock version | Apache-2.0 | Preserve its wheel license notice. |
| SenseVoiceSmall ASR | selected upstream snapshot | Custom FunASR model agreement in the current official model card; see `scripts/licenses/MODEL-LICENSE.md` | Include the complete applicable model agreement and author/source attribution; review the exact downloaded snapshot. |
| FSMN VAD | selected upstream snapshot | Apache-2.0 | Include Apache-2.0 notice. |
| CT-Transformer CN/EN punctuation | selected upstream snapshot | Apache-2.0 (cached model card) | Include this notice and retain cached model card. |
| CAM++ speaker model | selected upstream snapshot | Apache-2.0 | Include Apache-2.0 notice and upstream attribution. |

The FunASR toolkit license does **not** determine model-weight redistribution
rights. The model downloader writes an ID/revision/file-SHA manifest and keeps
the model cards in the bundled snapshot. Earlier cached model-card declarations
must not be generalized to every model or revision. In particular, do not
describe SenseVoiceSmall weights as MIT or Apache-2.0 based on the toolkit
license or this older inventory. Release review must retain each exact model
card, reconcile conversion/original-weight terms and re-check declarations
whenever a model revision changes.
