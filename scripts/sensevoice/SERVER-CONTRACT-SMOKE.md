# SenseVoice server HTTP 契约 smoke

先运行 `run-server-smoke.sh` 验证启动、loopback 监听和 `/health`。随后使用锁定
Q8、FSMN-VAD 和 `runtime/llama.cpp/tests/sample.wav` 发起 multipart 请求：

```bash
curl --fail --silent --show-error \
  -F "file=@runtime/llama.cpp/tests/sample.wav;type=audio/wav" \
  -F 'model=sensevoice-small' \
  -F 'response_format=verbose_json' \
  http://127.0.0.1:<port>/v1/audio/transcriptions > result.json
scripts/sensevoice/verify-verbose-json.sh --input result.json --min-segments 1
```

多段场景由 `generate-multi-segment-wav.sh` 从同一标准 PCM WAV 生成“语音—1.5 秒静音—语音”夹具；将其以相同 multipart 字段提交，并执行：

```bash
scripts/sensevoice/verify-verbose-json.sh --input result.json --min-segments 2 --min-gap-ms 1000
```

校验器要求非空 `text`、非负且不超过 `duration` 的段边界、秒/毫秒字段在 1ms 内一致，并要求段的秒与毫秒边界均单调不重叠；多段夹具额外要求至少 1 秒的静音间隙。它不把词级时间戳作为 GGUF 契约。
