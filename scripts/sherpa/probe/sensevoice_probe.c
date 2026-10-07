#include <errno.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include <mach/mach.h>
#include <malloc/malloc.h>

#include "sherpa-onnx/c-api/c-api.h"

typedef struct ProbeOptions {
  const char *model;
  const char *tokens;
  const char *wav;
  const char *language;
  int32_t use_itn;
  int32_t num_threads;
  int32_t debug;
  int32_t load_only;
  int32_t lifecycle_iterations;
} ProbeOptions;

static void PrintUsage(const char *program) {
  fprintf(stderr,
          "Usage: %s --model FILE --tokens FILE [--wav FILE] "
          "[--language auto] [--num-threads 1] [--no-itn] [--load-only] "
          "[--lifecycle-iterations N] [--debug]\n",
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
  options->language = "auto";
  options->use_itn = 1;
  options->num_threads = 1;

  for (i = 1; i < argc; ++i) {
    if (strcmp(argv[i], "--debug") == 0) {
      options->debug = 1;
    } else if (strcmp(argv[i], "--load-only") == 0) {
      options->load_only = 1;
    } else if (strcmp(argv[i], "--no-itn") == 0) {
      options->use_itn = 0;
    } else if (i + 1 < argc &&
               strcmp(argv[i], "--lifecycle-iterations") == 0) {
      if (!ParseInt32(argv[++i], &options->lifecycle_iterations)) return 0;
    } else if (i + 1 < argc && strcmp(argv[i], "--model") == 0) {
      options->model = argv[++i];
    } else if (i + 1 < argc && strcmp(argv[i], "--tokens") == 0) {
      options->tokens = argv[++i];
    } else if (i + 1 < argc && strcmp(argv[i], "--wav") == 0) {
      options->wav = argv[++i];
    } else if (i + 1 < argc && strcmp(argv[i], "--language") == 0) {
      options->language = argv[++i];
    } else if (i + 1 < argc && strcmp(argv[i], "--num-threads") == 0) {
      if (!ParseInt32(argv[++i], &options->num_threads)) {
        return 0;
      }
    } else {
      return 0;
    }
  }

  return options->model != NULL && options->tokens != NULL &&
         !(options->load_only && options->lifecycle_iterations > 0) &&
         (options->load_only || options->wav != NULL);
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
        codepoint > 0x10ffff || (codepoint >= 0xd800 && codepoint <= 0xdfff)) {
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

static double ElapsedMs(struct timespec start, struct timespec end) {
  return (double)(end.tv_sec - start.tv_sec) * 1000.0 +
         (double)(end.tv_nsec - start.tv_nsec) / 1000000.0;
}

static int HasValidTimeline(const SherpaOnnxOfflineRecognizerResult *result,
                            double audio_duration_seconds) {
  float previous = -1.0f;
  int32_t i;
  if (result->timestamps == NULL || result->count <= 0) return 0;
  for (i = 0; i < result->count; ++i) {
    float timestamp = result->timestamps[i];
    if (!isfinite(timestamp) || timestamp < 0.0f ||
        timestamp > audio_duration_seconds || timestamp < previous) {
      return 0;
    }
    previous = timestamp;
  }
  return 1;
}

static void FillRecognizerConfig(const ProbeOptions *options,
                                 SherpaOnnxOfflineRecognizerConfig *config) {
  memset(config, 0, sizeof(*config));
  config->decoding_method = "greedy_search";
  config->model_config.debug = options->debug;
  config->model_config.num_threads = options->num_threads;
  config->model_config.provider = "cpu";
  config->model_config.tokens = options->tokens;
  config->model_config.sense_voice.model = options->model;
  config->model_config.sense_voice.language = options->language;
  config->model_config.sense_voice.use_itn = options->use_itn;
}

static uint64_t CurrentRssBytes(void) {
  mach_task_basic_info_data_t info;
  mach_msg_type_number_t count = MACH_TASK_BASIC_INFO_COUNT;
  kern_return_t status = task_info(mach_task_self(), MACH_TASK_BASIC_INFO,
                                   (task_info_t)&info, &count);
  return status == KERN_SUCCESS ? (uint64_t)info.resident_size : 0;
}

static int RunLifecycleLoop(const ProbeOptions *options,
                            const SherpaOnnxWave *wave) {
  int32_t iteration;
  uint64_t baseline_rss = CurrentRssBytes();
  fprintf(stdout,
          "{\"schema_version\":1,\"lifecycle_iterations\":%d,"
          "\"baseline_rss_bytes\":%llu,\"iterations\":[",
          options->lifecycle_iterations, (unsigned long long)baseline_rss);
  for (iteration = 0; iteration < options->lifecycle_iterations; ++iteration) {
    SherpaOnnxOfflineRecognizerConfig config;
    const SherpaOnnxOfflineRecognizer *recognizer = NULL;
    const SherpaOnnxOfflineStream *stream = NULL;
    const SherpaOnnxOfflineRecognizerResult *result = NULL;
    struct timespec load_start, load_end, decode_start, decode_end;
    int ok = 0;

    FillRecognizerConfig(options, &config);
    clock_gettime(CLOCK_MONOTONIC, &load_start);
    recognizer = SherpaOnnxCreateOfflineRecognizer(&config);
    clock_gettime(CLOCK_MONOTONIC, &load_end);
    if (recognizer == NULL) goto iteration_cleanup;
    stream = SherpaOnnxCreateOfflineStream(recognizer);
    if (stream == NULL) goto iteration_cleanup;
    SherpaOnnxAcceptWaveformOffline(stream, wave->sample_rate, wave->samples,
                                    wave->num_samples);
    clock_gettime(CLOCK_MONOTONIC, &decode_start);
    SherpaOnnxDecodeOfflineStream(recognizer, stream);
    clock_gettime(CLOCK_MONOTONIC, &decode_end);
    result = SherpaOnnxGetOfflineStreamResult(stream);
    if (result == NULL || result->text == NULL || result->text[0] == '\0' ||
        !IsValidUtf8(result->text)) {
      goto iteration_cleanup;
    }
    ok = 1;

iteration_cleanup:
    if (result != NULL) SherpaOnnxDestroyOfflineRecognizerResult(result);
    if (stream != NULL) SherpaOnnxDestroyOfflineStream(stream);
    if (recognizer != NULL) SherpaOnnxDestroyOfflineRecognizer(recognizer);
    if (!ok) {
      fprintf(stderr, "probe_error=lifecycle_iteration_failed iteration=%d\n",
              iteration);
      return 1;
    }
    malloc_zone_pressure_relief(NULL, 0);
    if (iteration != 0) fputc(',', stdout);
    fprintf(stdout,
            "{\"index\":%d,\"load_ms\":%.3f,\"decode_ms\":%.3f,"
            "\"rss_after_destroy_bytes\":%llu}",
            iteration, ElapsedMs(load_start, load_end),
            ElapsedMs(decode_start, decode_end),
            (unsigned long long)CurrentRssBytes());
  }
  fputs("]}\n", stdout);
  return 0;
}

int main(int argc, char **argv) {
  ProbeOptions options;
  SherpaOnnxOfflineRecognizerConfig config;
  const SherpaOnnxOfflineRecognizer *recognizer = NULL;
  const SherpaOnnxOfflineStream *stream = NULL;
  const SherpaOnnxOfflineRecognizerResult *result = NULL;
  const SherpaOnnxWave *wave = NULL;
  struct timespec load_start, load_end, decode_start, decode_end;
  int32_t i;
  int exit_code = 1;

  if (!ParseOptions(argc, argv, &options)) {
    PrintUsage(argv[0]);
    return 2;
  }

  if (!options.load_only) {
    wave = SherpaOnnxReadWave(options.wav);
    if (wave == NULL || wave->sample_rate <= 0 || wave->num_samples <= 0) {
      fprintf(stderr, "probe_error=invalid_wav\n");
      goto cleanup;
    }
  }

  if (options.lifecycle_iterations > 0) {
    exit_code = RunLifecycleLoop(&options, wave);
    goto cleanup;
  }

  FillRecognizerConfig(&options, &config);

  clock_gettime(CLOCK_MONOTONIC, &load_start);
  recognizer = SherpaOnnxCreateOfflineRecognizer(&config);
  clock_gettime(CLOCK_MONOTONIC, &load_end);
  if (recognizer == NULL) {
    fprintf(stderr, "probe_error=recognizer_create_failed\n");
    goto cleanup;
  }

  if (options.load_only) {
    fprintf(stdout, "{\"schema_version\":1,\"load_only\":true,"
                    "\"load_ms\":%.3f}\n",
            ElapsedMs(load_start, load_end));
    exit_code = 0;
    goto cleanup;
  }

  stream = SherpaOnnxCreateOfflineStream(recognizer);
  if (stream == NULL) {
    fprintf(stderr, "probe_error=stream_create_failed\n");
    goto cleanup;
  }
  SherpaOnnxAcceptWaveformOffline(stream, wave->sample_rate, wave->samples,
                                  wave->num_samples);
  clock_gettime(CLOCK_MONOTONIC, &decode_start);
  SherpaOnnxDecodeOfflineStream(recognizer, stream);
  clock_gettime(CLOCK_MONOTONIC, &decode_end);
  result = SherpaOnnxGetOfflineStreamResult(stream);
  if (result == NULL || result->text == NULL || result->text[0] == '\0' ||
      !IsValidUtf8(result->text) || result->count < 0 ||
      (result->count > 0 && result->tokens_arr == NULL)) {
    fprintf(stderr, "probe_error=invalid_result\n");
    goto cleanup;
  }

  fputs("{\"schema_version\":1,\"text\":", stdout);
  PrintJsonString(result->text);
  fprintf(stdout,
          ",\"text_bytes\":%zu,\"sample_rate\":%d,\"sample_count\":%d,"
          "\"audio_duration_seconds\":%.6f,\"load_ms\":%.3f,"
          "\"decode_ms\":%.3f,\"token_count\":%d,"
          "\"timestamps_available\":%s,\"alignment_available\":%s,"
          "\"tokens\":[",
          strlen(result->text), wave->sample_rate, wave->num_samples,
          (double)wave->num_samples / wave->sample_rate,
          ElapsedMs(load_start, load_end), ElapsedMs(decode_start, decode_end),
          result->count, result->timestamps == NULL ? "false" : "true",
          HasValidTimeline(result,
                           (double)wave->num_samples / wave->sample_rate)
              ? "true"
              : "false");
  for (i = 0; i < result->count; ++i) {
    if (i != 0) fputc(',', stdout);
    PrintJsonString(result->tokens_arr[i]);
  }
  fputs("],\"timestamps\":", stdout);
  if (result->timestamps == NULL) {
    fputs("null", stdout);
  } else {
    fputc('[', stdout);
    for (i = 0; i < result->count; ++i) {
      if (i != 0) fputc(',', stdout);
      if (!isfinite(result->timestamps[i])) {
        fputs("null", stdout);
      } else {
        fprintf(stdout, "%.6f", result->timestamps[i]);
      }
    }
    fputc(']', stdout);
  }
  fputs(",\"durations_available\":", stdout);
  fputs(result->durations == NULL ? "false" : "true", stdout);
  fputs(",\"lang\":", stdout);
  PrintJsonString(result->lang);
  fputs(",\"emotion\":", stdout);
  PrintJsonString(result->emotion);
  fputs(",\"event\":", stdout);
  PrintJsonString(result->event);
  fputs("}\n", stdout);
  exit_code = 0;

cleanup:
  if (result != NULL) SherpaOnnxDestroyOfflineRecognizerResult(result);
  if (stream != NULL) SherpaOnnxDestroyOfflineStream(stream);
  if (recognizer != NULL) SherpaOnnxDestroyOfflineRecognizer(recognizer);
  if (wave != NULL) SherpaOnnxFreeWave(wave);
  return exit_code;
}
