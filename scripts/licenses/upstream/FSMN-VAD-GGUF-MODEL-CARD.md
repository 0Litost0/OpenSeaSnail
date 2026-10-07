---
license: apache-2.0
language:
- zh
- en
library_name: gguf
tags:
- voice-activity-detection
- vad
- fsmn
- funasr
- llama.cpp
- ggml
- cpu
- on-device
pipeline_tag: voice-activity-detection
---

# FSMN-VAD · GGUF (FunASR llama.cpp runtime)

GGUF build of FunASR's **FSMN-VAD** for the zero-Python, CPU/edge **[FunASR llama.cpp runtime](https://github.com/modelscope/FunASR/tree/main/runtime/llama.cpp)**. Native ggml voice-activity detection: segment long audio entirely in C++, no Python at runtime.

## Files
| file | size | notes |
|---|---|---|
| `fsmn-vad.gguf` | 1.7 MB | FSMN encoder + CMVN |

## Usage
Pass `--vad` to any FunASR llama.cpp tool to segment long audio internally:
```bash
llama-funasr-sensevoice -m sensevoice-small.gguf -a long.wav --vad fsmn-vad.gguf
llama-funasr-cli --enc funasr-encoder-f16.gguf -m qwen3-0.6b-q8_0.gguf -a long.wav --vad fsmn-vad.gguf
```
Segment boundaries match the PyTorch `fsmn-vad` front end within ~10 ms.

## Links
- 🧩 Runtime & build instructions: **[FunASR · runtime/llama.cpp](https://github.com/modelscope/FunASR/tree/main/runtime/llama.cpp)** — ⭐ **Star [FunASR](https://github.com/modelscope/FunASR) if this helps!**
- Source model: [funasr/fsmn-vad](https://huggingface.co/funasr/fsmn-vad)
