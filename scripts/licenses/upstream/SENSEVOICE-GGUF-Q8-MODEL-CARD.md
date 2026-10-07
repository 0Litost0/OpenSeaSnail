---
license: apache-2.0
language:
- zh
- en
library_name: gguf
tags:
- automatic-speech-recognition
- asr
- sensevoice
- funasr
- llama.cpp
- ggml
- cpu
- chinese
pipeline_tag: automatic-speech-recognition
---

# SenseVoiceSmall · GGUF (FunASR llama.cpp runtime)

GGUF build of **[SenseVoiceSmall](https://huggingface.co/FunAudioLLM/SenseVoiceSmall)** (SAN-M encoder + CTC) for the zero-Python, CPU/edge **[FunASR llama.cpp runtime](https://github.com/FunAudioLLM/SenseVoice/tree/main/runtime/llama.cpp)** — multilingual ASR with language / emotion / event tags, **~20× real-time on CPU**.

## Files
| file | size | notes |
|---|---|---|
| `sensevoice-small-f16.gguf` | 470 MB | **recommended** (f16 matmul weights) |
| `sensevoice-small.gguf` | 936 MB | f32 reference |

## Usage
The binary prints **transcription text** directly (no Python detok). `--ids` for raw ids / `--keep-tags` for the lang/emotion tags.
```bash
# 1. get the VAD too (for long audio): huggingface-cli download FunAudioLLM/fsmn-vad-GGUF
llama-funasr-sensevoice -m sensevoice-small-f16.gguf -a audio.wav --vad fsmn-vad.gguf
```
On CPU (8 threads) this reaches **8.01 % CER** on the 184-clip Mandarin benchmark — vs whisper.cpp 22–31 %. See the [benchmark](https://github.com/FunAudioLLM/SenseVoice/blob/main/runtime/llama.cpp/BENCHMARKS.md).

## Links
- 🧩 Runtime & build: **[SenseVoice · runtime/llama.cpp](https://github.com/FunAudioLLM/SenseVoice/tree/main/runtime/llama.cpp)** — ⭐ **Star [SenseVoice](https://github.com/FunAudioLLM/SenseVoice)!**
- Source model: [FunAudioLLM/SenseVoiceSmall](https://huggingface.co/FunAudioLLM/SenseVoiceSmall)
