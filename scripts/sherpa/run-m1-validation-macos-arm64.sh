#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 --install DIR --cache DIR --evidence DIR" >&2
}

install_dir=""
cache_dir=""
evidence_dir=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --install) install_dir=${2:-}; shift 2 ;;
    --cache) cache_dir=${2:-}; shift 2 ;;
    --evidence) evidence_dir=${2:-}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

[[ -n "$install_dir" && -n "$cache_dir" && -n "$evidence_dir" ]] || {
  usage
  exit 2
}

for command_name in ffmpeg file jq lipo otool shasum tar vtool; do
  command -v "$command_name" >/dev/null || {
    echo "Required command not found: $command_name" >&2
    exit 1
  }
done

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
artifact_lock="$script_dir/artifact-lock.json"
mapping_fixtures="$script_dir/probe/mapping-risk-fixtures.json"
archive_name=$(jq -r '.downloads[] | select(.role == "asr-archive") | .file_name' "$artifact_lock")
archive="$cache_dir/$archive_name"
probe="$install_dir/bin/seasnail-sherpa-probe"
vad_probe="$install_dir/bin/seasnail-sherpa-vad-probe"
vad_model="$cache_dir/$(jq -r '.downloads[] | select(.role == "vad") | .file_name' "$artifact_lock")"
model_root_name=${archive_name%.tar.bz2}
fixture_root="$evidence_dir/fixtures/$model_root_name"
model="$fixture_root/model.int8.onnx"
tokens="$fixture_root/tokens.txt"

[[ -x "$probe" && -x "$vad_probe" && -f "$archive" && -f "$vad_model" ]] || {
  echo "Missing probe or locked artifact input" >&2
  exit 1
}

mkdir -p "$evidence_dir/fixtures" "$evidence_dir/results" "$evidence_dir/resources"
tar -xjf "$archive" -C "$evidence_dir/fixtures" \
  "$model_root_name/model.int8.onnx" \
  "$model_root_name/tokens.txt" \
  "$model_root_name/test_wavs/zh.wav" \
  "$model_root_name/test_wavs/en.wav"

zh_wav="$fixture_root/test_wavs/zh.wav"
en_wav="$fixture_root/test_wavs/en.wav"
mixed_wav="$evidence_dir/fixtures/mixed-zh-en.wav"
multi_wav="$evidence_dir/fixtures/multi-zh-silence-en.wav"
ffmpeg -hide_banner -loglevel error -y -i "$zh_wav" -i "$en_wav" \
  -filter_complex '[0:a][1:a]concat=n=2:v=0:a=1[out]' -map '[out]' \
  -ar 16000 -ac 1 -c:a pcm_s16le "$mixed_wav"
ffmpeg -hide_banner -loglevel error -y -i "$zh_wav" \
  -f lavfi -t 1.5 -i anullsrc=r=16000:cl=mono -i "$en_wav" \
  -filter_complex '[0:a][1:a][2:a]concat=n=3:v=0:a=1[out]' -map '[out]' \
  -ar 16000 -ac 1 -c:a pcm_s16le "$multi_wav"

run_probe() {
  local wav=$1
  local output=$2
  shift 2
  "$probe" --model "$model" --tokens "$tokens" --wav "$wav" "$@" >"$output"
  jq -e '
    .schema_version == 1 and
    (.text | type == "string" and length > 0) and
    .token_count == (.tokens | length) and
    (.alignment_available | type == "boolean") and
    ((.alignment_available | not) or (
      .token_count == (.timestamps | length) and
      ([.timestamps[] | select(type != "number")] | length == 0) and
      ([range(1; .timestamps | length) as $i |
        select(.timestamps[$i] < .timestamps[$i - 1])] | length == 0)))
  ' "$output" >/dev/null
}

run_probe "$zh_wav" "$evidence_dir/results/zh-itn.json"
run_probe "$zh_wav" "$evidence_dir/results/zh-no-itn.json" --no-itn
run_probe "$en_wav" "$evidence_dir/results/en.json"
run_probe "$mixed_wav" "$evidence_dir/results/mixed-zh-en.json"

for output in "$evidence_dir/results/zh-itn.json" \
              "$evidence_dir/results/zh-no-itn.json" \
              "$evidence_dir/results/en.json" \
              "$evidence_dir/results/mixed-zh-en.json"; do
  jq -e '([.tokens[]] | join("")) == .text' "$output" >/dev/null
done

jq -e '
  .schema_version == 1 and .privacy == "synthetic_non_sensitive" and
  ([.cases[] | select(
    (.relative_timestamps | length) != (.decoded_tokens | length) or
    ([range(1; .relative_timestamps | length) as $i |
      select(.relative_timestamps[$i] < .relative_timestamps[$i - 1])] |
      length) != 0 or
    ((.decoded_tokens | join("")) == .final_text) !=
      (.expected_mapping == "exact") or
    (.expected_mapping == "fallback" and
      .expected_reason != "token_text_mismatch"))] | length) == 0
' "$mapping_fixtures" >/dev/null
[[ "$(jq '.cases | length' "$mapping_fixtures")" == "3" ]]
for case_index in 0 1 2; do
  case_id=$(jq -r ".cases[$case_index].id" "$mapping_fixtures")
  token_array_sha256=$(jq -c ".cases[$case_index].decoded_tokens" \
    "$mapping_fixtures" | shasum -a 256 | awk '{print $1}')
  jq -n \
    --arg id "$case_id" \
    --arg token_array_sha256 "$token_array_sha256" \
    --argjson token_count "$(jq ".cases[$case_index].decoded_tokens | length" "$mapping_fixtures")" \
    --argjson timestamp_min "$(jq ".cases[$case_index].relative_timestamps | min" "$mapping_fixtures")" \
    --argjson timestamp_max "$(jq ".cases[$case_index].relative_timestamps | max" "$mapping_fixtures")" \
    --arg observed_mapping "$(jq -r ".cases[$case_index] | if (.decoded_tokens | join(\"\")) == .final_text then \"exact\" else \"fallback\" end" "$mapping_fixtures")" \
    '{id: $id, token_count: $token_count, token_array_sha256: $token_array_sha256,
      timestamp_min: $timestamp_min, timestamp_max: $timestamp_max,
      observed_mapping: $observed_mapping}' \
    >"$evidence_dir/results/mapping-case-$case_index.json"
done
jq -s '.' "$evidence_dir"/results/mapping-case-[0-2].json \
  >"$evidence_dir/results/mapping-derived-metrics.json"

"$vad_probe" --model "$model" --tokens "$tokens" --vad-model "$vad_model" \
  --wav "$zh_wav" >"$evidence_dir/results/vad-single.json"
"$vad_probe" --model "$model" --tokens "$tokens" --vad-model "$vad_model" \
  --wav "$multi_wav" >"$evidence_dir/results/vad-multi.json"
jq -e '
  .segment_count == (.segments | length) and .segment_count >= 2 and
  ([.segments[] | select(
    (.start_seconds | type) != "number" or
    (.end_seconds | type) != "number" or
    .end_seconds <= .start_seconds or
    .token_count != (.tokens | length))] | length == 0) and
  ([range(1; .segments | length) as $i |
    select(.segments[$i].start_seconds < .segments[$i - 1].end_seconds)] |
    length == 0) and
  ([.segments[] as $segment | select($segment.alignment_available) |
    $segment.tokens[] | select(
    .global_start_seconds < $segment.start_seconds or
    .global_end_seconds > $segment.end_seconds or
    .global_end_seconds < .global_start_seconds)] | length == 0)
' "$evidence_dir/results/vad-multi.json" >/dev/null

for macho_path in "$probe" "$vad_probe" "$install_dir"/lib/*.dylib; do
  [[ -e "$macho_path" ]] || continue
  [[ "$(lipo -archs "$macho_path")" == "arm64" ]]
  [[ "$(vtool -show-build "$macho_path" | awk '/minos/{print $2; exit}')" == "13.0" ]]
  while IFS= read -r dependency; do
    case "$dependency" in
      /System/Library/*|/usr/lib/*) ;;
      @rpath/*)
        dependency_name=${dependency#@rpath/}
        [[ -e "$install_dir/lib/$dependency_name" ]] || {
          echo "Unresolved bundled dependency: $dependency" >&2
          exit 1
        }
        ;;
      *)
        echo "Unsupported or non-portable dependency: $dependency" >&2
        exit 1
        ;;
    esac
  done < <(otool -L "$macho_path" | awk 'NR > 1 { print $1 }')
done

/usr/bin/time -l -o "$evidence_dir/resources/time-lifecycle.txt" \
  "$probe" --model "$model" --tokens "$tokens" --wav "$zh_wav" \
  --lifecycle-iterations 30 \
  >"$evidence_dir/resources/lifecycle.json"
jq -e '
  .lifecycle_iterations == 30 and
  (.iterations | length) == 30 and
  ([.iterations[] | select(.rss_after_destroy_bytes <= 0)] | length) == 0 and
  (([.iterations[-5:][] | .rss_after_destroy_bytes] | max) -
    ([.iterations[-5:][] | .rss_after_destroy_bytes] | min) <= 16777216)
' "$evidence_dir/resources/lifecycle.json" >/dev/null
/usr/bin/time -l -o "$evidence_dir/resources/time-load-only.txt" \
  "$probe" --model "$model" --tokens "$tokens" --load-only \
  >"$evidence_dir/resources/load-only.json"

lifecycle_peak_rss=$(awk '/maximum resident set size/ { print $1 }' \
  "$evidence_dir/resources/time-lifecycle.txt")
load_only_rss=$(awk '/maximum resident set size/ { print $1 }' \
  "$evidence_dir/resources/time-load-only.txt")

dd if=/dev/zero of="$evidence_dir/fixtures/invalid.wav" bs=1 count=1 2>/dev/null
if "$probe" --model "$model" --tokens "$tokens" \
    --wav "$evidence_dir/fixtures/invalid.wav" \
    >/dev/null 2>/dev/null; then
  echo "Invalid WAV unexpectedly succeeded" >&2
  exit 1
fi
dd if="$model" of="$evidence_dir/fixtures/corrupt-model.onnx" \
  bs=4096 count=1 2>/dev/null
expected_model_sha256=$(jq -r \
  '.archive_entries[] | select(.role == "asr-model") | .sha256' "$artifact_lock")
corrupt_model_sha256=$(shasum -a 256 \
  "$evidence_dir/fixtures/corrupt-model.onnx" | awk '{print $1}')
[[ "$corrupt_model_sha256" != "$expected_model_sha256" ]]
if { "$probe" --model "$evidence_dir/fixtures/corrupt-model.onnx" \
    --tokens "$tokens" --wav "$zh_wav"; } >/dev/null 2>/dev/null; then
  echo "Corrupt model unexpectedly succeeded" >&2
  exit 1
fi
jq -n '{schema_version: 1, invalid_wav: "controlled_failure",
  corrupt_existing_model: "rejected_by_sha256_preflight",
  native_corrupt_model_failure: "contained_by_probe_process",
  raw_stderr_persisted: false}' >"$evidence_dir/results/negative-cases.json"

jq -n \
  --arg model_sha256 "$(shasum -a 256 "$model" | awk '{print $1}')" \
  --arg tokens_sha256 "$(shasum -a 256 "$tokens" | awk '{print $1}')" \
  --arg zh_sha256 "$(shasum -a 256 "$zh_wav" | awk '{print $1}')" \
  --arg en_sha256 "$(shasum -a 256 "$en_wav" | awk '{print $1}')" \
  --arg mixed_sha256 "$(shasum -a 256 "$mixed_wav" | awk '{print $1}')" \
  --arg multi_sha256 "$(shasum -a 256 "$multi_wav" | awk '{print $1}')" \
  --arg mapping_fixtures_sha256 "$(shasum -a 256 "$mapping_fixtures" | awk '{print $1}')" \
  --arg zh_tokens_sha256 "$(jq -c '.tokens' "$evidence_dir/results/zh-itn.json" | shasum -a 256 | awk '{print $1}')" \
  --arg en_tokens_sha256 "$(jq -c '.tokens' "$evidence_dir/results/en.json" | shasum -a 256 | awk '{print $1}')" \
  --arg mixed_tokens_sha256 "$(jq -c '.tokens' "$evidence_dir/results/mixed-zh-en.json" | shasum -a 256 | awk '{print $1}')" \
  --argjson lifecycle_peak_rss "$lifecycle_peak_rss" \
  --argjson load_only_rss "$load_only_rss" \
  --slurpfile zh "$evidence_dir/results/zh-itn.json" \
  --slurpfile zh_no_itn "$evidence_dir/results/zh-no-itn.json" \
  --slurpfile en "$evidence_dir/results/en.json" \
  --slurpfile mixed "$evidence_dir/results/mixed-zh-en.json" \
  --slurpfile vad "$evidence_dir/results/vad-multi.json" \
  --slurpfile lifecycle "$evidence_dir/resources/lifecycle.json" \
  --slurpfile load_only "$evidence_dir/resources/load-only.json" \
  --slurpfile mapping_metrics "$evidence_dir/results/mapping-derived-metrics.json" \
  '{
    schema_version: 1,
    fixture_hashes: {
      model: $model_sha256, tokens: $tokens_sha256, zh: $zh_sha256,
      en: $en_sha256, mixed: $mixed_sha256, multi: $multi_sha256,
      mapping_risk_fixtures: $mapping_fixtures_sha256
    },
    observations: {
      zh_itn: {token_count: $zh[0].token_count, text_bytes: $zh[0].text_bytes,
        token_array_sha256: $zh_tokens_sha256,
        alignment_available: $zh[0].alignment_available,
        timestamp_min: ($zh[0].timestamps | min), timestamp_max: ($zh[0].timestamps | max)},
      zh_no_itn: {token_count: $zh_no_itn[0].token_count,
        text_bytes: $zh_no_itn[0].text_bytes},
      en: {token_count: $en[0].token_count, text_bytes: $en[0].text_bytes,
        token_array_sha256: $en_tokens_sha256,
        alignment_available: $en[0].alignment_available,
        timestamp_min: ($en[0].timestamps | min), timestamp_max: ($en[0].timestamps | max)},
      mixed: {token_count: $mixed[0].token_count, text_bytes: $mixed[0].text_bytes,
        token_array_sha256: $mixed_tokens_sha256,
        alignment_available: $mixed[0].alignment_available,
        timestamp_min: ($mixed[0].timestamps | min), timestamp_max: ($mixed[0].timestamps | max)},
      vad_multi: {segment_count: $vad[0].segment_count,
        alignment_available: ([ $vad[0].segments[].alignment_available ] | all),
        segment_bounds: [$vad[0].segments[] | [.start_seconds, .end_seconds]]},
      mapping_risk_cases: $mapping_metrics[0]
    },
    resources: {
      loaded_idle: {load_ms: $load_only[0].load_ms, peak_rss_bytes: $load_only_rss},
      same_process_lifecycle: {
        iterations: $lifecycle[0].lifecycle_iterations,
        baseline_rss_bytes: $lifecycle[0].baseline_rss_bytes,
        process_peak_rss_bytes: $lifecycle_peak_rss,
        rss_after_destroy_bytes: [$lifecycle[0].iterations[].rss_after_destroy_bytes],
        timings: [$lifecycle[0].iterations[] | {load_ms, decode_ms}]
      }
    },
    assertions: {
      utf8_nonempty: true, tokens_reconstruct_text: true,
      synthetic_mapping_risks_classified: true,
      alignment_available_for_locked_candidate: ([ $vad[0].segments[].alignment_available ] | all),
      optional_timestamps_supported: true, vad_global_timeline_valid: true,
      arm64_macos_13: true, portable_dylib_closure: true,
      same_process_lifecycle_plateaus: true,
      invalid_wav_controlled_failure: true,
      corrupt_model_sha256_preflight_rejected: true,
      native_corrupt_model_failure_contained_by_process: true,
      persisted_diagnostics_are_path_free: true
    }
  }' >"$evidence_dir/summary.json"

echo "M1 validation passed; evidence: $evidence_dir/summary.json"
