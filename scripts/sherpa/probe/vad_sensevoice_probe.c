#include <errno.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "sherpa-onnx/c-api/c-api.h"

typedef struct ProbeOptions {
  const char *model;
  const char *tokens;
  const char *vad_model;
  const char *wav;
  int32_t num_threads;
  int32_t debug;
} ProbeOptions;

static void PrintUsage(const char *program) {
  fprintf(stderr,
          "Usage: %s --model FILE --tokens FILE --vad-model FILE --wav FILE "
          "[--num-threads 1] [--debug]\n",
          program);
}

static int ParseInt32(const char *value, int32_t *result) {
  char *end = NULL;
  long parsed;
  errno = 0;
  parsed = strtol(value, &end, 10);
  if (errno != 0 || end == value || *end != '\0' || parsed < 1 ||
      parsed > INT32_MAX) {
    return 0;
  }
  *result = (int32_t)parsed;
  return 1;
}

static int ParseOptions(int argc, char **argv, ProbeOptions *options) {
  int i;
  memset(options, 0, sizeof(*options));
  options->num_threads = 1;
  for (i = 1; i < argc; ++i) {
    if (strcmp(argv[i], "--debug") == 0) {
      options->debug = 1;
    } else if (i + 1 < argc && strcmp(argv[i], "--model") == 0) {
      options->model = argv[++i];
    } else if (i + 1 < argc && strcmp(argv[i], "--tokens") == 0) {
      options->tokens = argv[++i];
    } else if (i + 1 < argc && strcmp(argv[i], "--vad-model") == 0) {
      options->vad_model = argv[++i];
    } else if (i + 1 < argc && strcmp(argv[i], "--wav") == 0) {
      options->wav = argv[++i];
    } else if (i + 1 < argc && strcmp(argv[i], "--num-threads") == 0) {
      if (!ParseInt32(argv[++i], &options->num_threads)) return 0;
    } else {
      return 0;
    }
  }
  return options->model != NULL && options->tokens != NULL &&
         options->vad_model != NULL && options->wav != NULL;
}

static int IsValidUtf8(const char *text) {
  const unsigned char *p = (const unsigned char *)text;
  while (*p != '\0') {
    uint32_t codepoint;
    int continuation_count;
    int i;
    if (*p <= 0x7f) {
      ++p;
      continue;
    }
    if ((*p & 0xe0) == 0xc0) {
      codepoint = *p & 0x1f;
      continuation_count = 1;
      if (codepoint == 0) return 0;
    } else if ((*p & 0xf0) == 0xe0) {
      codepoint = *p & 0x0f;
      continuation_count = 2;
    } else if ((*p & 0xf8) == 0xf0) {
      codepoint = *p & 0x07;
      continuation_count = 3;
    } else {
      return 0;
    }
    ++p;
    for (i = 0; i < continuation_count; ++i) {
      if ((p[i] & 0xc0) != 0x80) return 0;
      codepoint = (codepoint << 6) | (p[i] & 0x3f);
    }
    if ((continuation_count == 1 && codepoint < 0x80) ||
        (continuation_count == 2 && codepoint < 0x800) ||
        (continuation_count == 3 && codepoint < 0x10000) ||
        codepoint > 0x10ffff ||
        (codepoint >= 0xd800 && codepoint <= 0xdfff)) {
      return 0;
    }
    p += continuation_count;
  }
  return 1;
}

static void PrintJsonString(const char *value) {
  const unsigned char *p;
  if (value == NULL) {
    fputs("null", stdout);
    return;
  }
  fputc('"', stdout);
  for (p = (const unsigned char *)value; *p != '\0'; ++p) {
    switch (*p) {
      case '"': fputs("\\\"", stdout); break;
      case '\\': fputs("\\\\", stdout); break;
      case '\b': fputs("\\b", stdout); break;
      case '\f': fputs("\\f", stdout); break;
      case '\n': fputs("\\n", stdout); break;
      case '\r': fputs("\\r", stdout); break;
      case '\t': fputs("\\t", stdout); break;
      default:
        if (*p < 0x20) {
          fprintf(stdout, "\\u%04x", (unsigned int)*p);
        } else {
          fputc(*p, stdout);
        }
    }
  }
  fputc('"', stdout);
}

static int PrintDecodedSegment(const SherpaOnnxOfflineRecognizer *recognizer,
                               const SherpaOnnxSpeechSegment *segment,
                               int32_t sample_rate, int32_t segment_index) {
  const SherpaOnnxOfflineStream *stream = NULL;
  const SherpaOnnxOfflineRecognizerResult *result = NULL;
  double segment_start = (double)segment->start / sample_rate;
  double segment_end = (double)(segment->start + segment->n) / sample_rate;
  double previous_start = -1.0;
  int alignment_available;
  int32_t i;
  int ok = 0;

  if (segment->start < 0 || segment->n <= 0 || segment->samples == NULL ||
      !isfinite(segment_start) || !isfinite(segment_end) ||
      segment_end <= segment_start) {
    fprintf(stderr, "probe_error=invalid_vad_segment index=%d\n", segment_index);
    return 0;
  }
  stream = SherpaOnnxCreateOfflineStream(recognizer);
  if (stream == NULL) {
    fprintf(stderr, "probe_error=stream_create_failed index=%d\n", segment_index);
    return 0;
  }
  SherpaOnnxAcceptWaveformOffline(stream, sample_rate, segment->samples,
                                  segment->n);
  SherpaOnnxDecodeOfflineStream(recognizer, stream);
  result = SherpaOnnxGetOfflineStreamResult(stream);
  if (result == NULL || result->text == NULL || result->text[0] == '\0' ||
      !IsValidUtf8(result->text) || result->count <= 0 ||
      result->tokens_arr == NULL) {
    fprintf(stderr, "probe_error=invalid_segment_result index=%d\n",
            segment_index);
    goto cleanup;
  }

  alignment_available = result->timestamps != NULL;
  for (i = 0; alignment_available && i < result->count; ++i) {
    double relative_start = result->timestamps[i];
    double global_start = segment_start + relative_start;
    double global_end = i + 1 < result->count
                            ? segment_start + result->timestamps[i + 1]
                            : segment_end;
    if (!isfinite(relative_start) || !isfinite(global_start) ||
        !isfinite(global_end) || relative_start < 0.0 ||
        global_start < segment_start || global_start > segment_end ||
        global_start < previous_start || global_end < global_start ||
        global_end > segment_end) {
      alignment_available = 0;
    }
    previous_start = global_start;
  }
  previous_start = -1.0;

  fprintf(stdout,
          "{\"index\":%d,\"start_sample\":%d,\"sample_count\":%d,"
          "\"start_seconds\":%.6f,\"end_seconds\":%.6f,\"text\":",
          segment_index, segment->start, segment->n, segment_start, segment_end);
  PrintJsonString(result->text);
  fprintf(stdout, ",\"token_count\":%d,\"alignment_available\":%s,"
                  "\"tokens\":[",
          result->count, alignment_available ? "true" : "false");
  for (i = 0; i < result->count; ++i) {
    if (i != 0) fputc(',', stdout);
    if (!alignment_available) {
      fputs("{\"text\":", stdout);
      PrintJsonString(result->tokens_arr[i]);
      fputc('}', stdout);
      continue;
    }
    double relative_start = result->timestamps[i];
    double global_start = segment_start + relative_start;
    double global_end = segment_end;
    if (i + 1 < result->count) {
      global_end = segment_start + result->timestamps[i + 1];
    }
    fputs("{\"text\":", stdout);
    PrintJsonString(result->tokens_arr[i]);
    fprintf(stdout,
            ",\"relative_start_seconds\":%.6f,"
            "\"global_start_seconds\":%.6f,\"global_end_seconds\":%.6f}",
            relative_start, global_start, global_end);
    previous_start = global_start;
  }
  fputs("]}", stdout);
  ok = 1;

cleanup:
  if (result != NULL) SherpaOnnxDestroyOfflineRecognizerResult(result);
  if (stream != NULL) SherpaOnnxDestroyOfflineStream(stream);
  return ok;
}

int main(int argc, char **argv) {
  ProbeOptions options;
  SherpaOnnxOfflineRecognizerConfig recognizer_config;
  SherpaOnnxVadModelConfig vad_config;
  const SherpaOnnxOfflineRecognizer *recognizer = NULL;
  const SherpaOnnxVoiceActivityDetector *vad = NULL;
  const SherpaOnnxWave *wave = NULL;
  int32_t offset;
  int32_t segment_count = 0;
  int exit_code = 1;

  if (!ParseOptions(argc, argv, &options)) {
    PrintUsage(argv[0]);
    return 2;
  }
  wave = SherpaOnnxReadWave(options.wav);
  if (wave == NULL || wave->sample_rate != 16000 || wave->num_samples <= 0) {
    fprintf(stderr, "probe_error=invalid_wav_expected_mono_16khz\n");
    goto cleanup;
  }

  memset(&recognizer_config, 0, sizeof(recognizer_config));
  recognizer_config.decoding_method = "greedy_search";
  recognizer_config.model_config.debug = options.debug;
  recognizer_config.model_config.num_threads = options.num_threads;
  recognizer_config.model_config.provider = "cpu";
  recognizer_config.model_config.tokens = options.tokens;
  recognizer_config.model_config.sense_voice.model = options.model;
  recognizer_config.model_config.sense_voice.language = "auto";
  recognizer_config.model_config.sense_voice.use_itn = 1;
  recognizer = SherpaOnnxCreateOfflineRecognizer(&recognizer_config);
  if (recognizer == NULL) {
    fprintf(stderr, "probe_error=recognizer_create_failed\n");
    goto cleanup;
  }

  memset(&vad_config, 0, sizeof(vad_config));
  vad_config.silero_vad.model = options.vad_model;
  vad_config.silero_vad.threshold = 0.5f;
  vad_config.silero_vad.min_silence_duration = 0.5f;
  vad_config.silero_vad.min_speech_duration = 0.25f;
  vad_config.silero_vad.max_speech_duration = 20.0f;
  vad_config.silero_vad.window_size = 512;
  vad_config.sample_rate = 16000;
  vad_config.num_threads = options.num_threads;
  vad_config.provider = "cpu";
  vad_config.debug = options.debug;
  vad = SherpaOnnxCreateVoiceActivityDetector(&vad_config, 120.0f);
  if (vad == NULL) {
    fprintf(stderr, "probe_error=vad_create_failed\n");
    goto cleanup;
  }

  for (offset = 0; offset < wave->num_samples; offset += 512) {
    int32_t n = wave->num_samples - offset;
    if (n > 512) n = 512;
    SherpaOnnxVoiceActivityDetectorAcceptWaveform(vad, wave->samples + offset,
                                                  n);
  }
  SherpaOnnxVoiceActivityDetectorFlush(vad);

  fprintf(stdout,
          "{\"schema_version\":1,\"sample_rate\":%d,"
          "\"sample_count\":%d,\"audio_duration_seconds\":%.6f,"
          "\"segments\":[",
          wave->sample_rate, wave->num_samples,
          (double)wave->num_samples / wave->sample_rate);
  while (!SherpaOnnxVoiceActivityDetectorEmpty(vad)) {
    const SherpaOnnxSpeechSegment *segment =
        SherpaOnnxVoiceActivityDetectorFront(vad);
    if (segment == NULL) {
      fprintf(stderr, "probe_error=vad_front_failed\n");
      goto cleanup;
    }
    if (segment_count != 0) fputc(',', stdout);
    if (!PrintDecodedSegment(recognizer, segment, wave->sample_rate,
                             segment_count)) {
      SherpaOnnxDestroySpeechSegment(segment);
      goto cleanup;
    }
    SherpaOnnxDestroySpeechSegment(segment);
    SherpaOnnxVoiceActivityDetectorPop(vad);
    ++segment_count;
  }
  if (segment_count == 0) {
    fprintf(stderr, "probe_error=no_vad_segments\n");
    goto cleanup;
  }
  fprintf(stdout, "],\"segment_count\":%d}\n", segment_count);
  exit_code = 0;

cleanup:
  if (vad != NULL) SherpaOnnxDestroyVoiceActivityDetector(vad);
  if (recognizer != NULL) SherpaOnnxDestroyOfflineRecognizer(recognizer);
  if (wave != NULL) SherpaOnnxFreeWave(wave);
  return exit_code;
}
