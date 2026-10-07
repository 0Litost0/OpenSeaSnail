#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: prepare-macos-arm64.sh --cache DIR --native-install DIR --output DIR \
  --source DIR --onnxruntime-source DIR

Prepare one verified Sherpa ONNX SenseVoice int8 artifact from offline inputs.
`--native-install` is the install directory produced by
build-probe-macos-arm64.sh. The output directory must not already exist.
EOF
}

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cache_dir=""
native_install=""
output_dir=""
source_dir=""
onnxruntime_source_dir=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --cache) [[ $# -ge 2 ]] || { usage; exit 2; }; cache_dir=$2; shift 2 ;;
    --native-install) [[ $# -ge 2 ]] || { usage; exit 2; }; native_install=$2; shift 2 ;;
    --output) [[ $# -ge 2 ]] || { usage; exit 2; }; output_dir=$2; shift 2 ;;
    --source) [[ $# -ge 2 ]] || { usage; exit 2; }; source_dir=$2; shift 2 ;;
    --onnxruntime-source) [[ $# -ge 2 ]] || { usage; exit 2; }; onnxruntime_source_dir=$2; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

[[ -n "$cache_dir" && -n "$native_install" && -n "$output_dir" && -n "$source_dir" && -n "$onnxruntime_source_dir" ]] || { usage; exit 2; }
[[ -d "$cache_dir" ]] || { echo "Cache directory not found: $cache_dir" >&2; exit 1; }
[[ -d "$native_install" ]] || { echo "Native install directory not found: $native_install" >&2; exit 1; }
[[ ! -e "$output_dir" ]] || { echo "Refusing to overwrite existing output: $output_dir" >&2; exit 1; }

for command_name in jq shasum stat tar lipo otool; do
  command -v "$command_name" >/dev/null || {
    echo "Required command not found: $command_name" >&2
    exit 1
  }
done

"$script_dir/verify-locks.sh" \
  --cache "$cache_dir" \
  --source "$source_dir" \
  --onnxruntime-source "$onnxruntime_source_dir"

artifact_lock="$script_dir/artifact-lock.json"
archive_name=$(jq -er '.downloads[] | select(.id == "sensevoice-int8-archive") | .file_name' "$artifact_lock")
archive="$cache_dir/$archive_name"
[[ -f "$archive" && ! -L "$archive" ]] || { echo "Missing locked archive: $archive" >&2; exit 1; }

mkdir -p "$output_dir/model" "$output_dir/lib" "$output_dir/bin" "$output_dir/licenses"

extract_entry() {
  local role=$1
  local destination=$2
  local archive_path expected_size expected_sha
  archive_path=$(jq -er --arg role "$role" '.archive_entries[] | select(.role == $role) | .archive_path' "$artifact_lock")
  expected_size=$(jq -er --arg role "$role" '.archive_entries[] | select(.role == $role) | .size_bytes' "$artifact_lock")
  expected_sha=$(jq -er --arg role "$role" '.archive_entries[] | select(.role == $role) | .sha256' "$artifact_lock")
  tar -xOjf "$archive" "$archive_path" > "$destination"
  [[ "$(stat -f '%z' "$destination")" == "$expected_size" ]] || {
    echo "Extracted size mismatch for $role" >&2; exit 1;
  }
  [[ "$(shasum -a 256 "$destination" | awk '{print $1}')" == "$expected_sha" ]] || {
    echo "Extracted SHA-256 mismatch for $role" >&2; exit 1;
  }
}

extract_entry asr-model "$output_dir/model/model.int8.onnx"
extract_entry tokens "$output_dir/model/tokens.txt"
extract_entry model-license-pointer "$output_dir/licenses/LICENSE.model-source"

vad_archive="$cache_dir/$(jq -er '.downloads[] | select(.id == "silero-vad") | .file_name' "$artifact_lock")"
vad_size=$(jq -er '.downloads[] | select(.id == "silero-vad") | .size_bytes' "$artifact_lock")
vad_sha=$(jq -er '.downloads[] | select(.id == "silero-vad") | .sha256' "$artifact_lock")
[[ -f "$vad_archive" && ! -L "$vad_archive" ]] || { echo "Missing locked VAD: $vad_archive" >&2; exit 1; }
cp -L "$vad_archive" "$output_dir/model/silero_vad.onnx"
[[ "$(stat -f '%z' "$output_dir/model/silero_vad.onnx")" == "$vad_size" ]] || { echo "VAD size mismatch" >&2; exit 1; }
[[ "$(shasum -a 256 "$output_dir/model/silero_vad.onnx" | awk '{print $1}')" == "$vad_sha" ]] || { echo "VAD SHA-256 mismatch" >&2; exit 1; }

copy_native() {
  local source=$1
  local destination=$2
  [[ -f "$source" && ! -L "$source" ]] || { echo "Missing native file: $source" >&2; exit 1; }
  cp -L "$source" "$destination"
  chmod u+rw,go+r "$destination"
}

copy_native "$native_install/lib/libonnxruntime.1.27.1.dylib" "$output_dir/lib/libonnxruntime.1.dylib"
copy_native "$native_install/lib/libsherpa-onnx-c-api.dylib" "$output_dir/lib/libsherpa-onnx-c-api.dylib"
copy_native "$native_install/bin/seasnail-sherpa-sidecar" "$output_dir/bin/seasnail-sherpa-sidecar"
chmod 755 "$output_dir/bin/seasnail-sherpa-sidecar"

for native in \
  "$output_dir/bin/seasnail-sherpa-sidecar" \
  "$output_dir/lib/libsherpa-onnx-c-api.dylib" \
  "$output_dir/lib/libonnxruntime.1.dylib"; do
  [[ "$(lipo -archs "$native")" == "arm64" ]] || { echo "Native file is not thin arm64: $native" >&2; exit 1; }
  while IFS= read -r dependency; do
    case "$dependency" in
      /usr/lib/*|/System/Library/*|@rpath/*|@loader_path/*) ;;
      *) echo "Unsupported native dependency: $dependency" >&2; exit 1 ;;
    esac
  done < <(otool -L "$native" | sed -n '2,$p' | awk '{print $1}')
done

cp -L "$source_dir/LICENSE" "$output_dir/licenses/SHERPA-ONNX-APACHE-2.0.txt"
cp -L "$onnxruntime_source_dir/LICENSE" "$output_dir/licenses/ONNXRUNTIME-MIT.txt"
cp -L "$onnxruntime_source_dir/ThirdPartyNotices.txt" "$output_dir/licenses/ONNXRUNTIME-THIRD-PARTY-NOTICES.txt"
cp "$script_dir/licenses/FUNASR-MIT.txt" "$output_dir/licenses/FUNASR-MIT.txt"
cp "$script_dir/licenses/NLOHMANN-JSON-MIT.txt" "$output_dir/licenses/NLOHMANN-JSON-MIT.txt"
cp "$script_dir/licenses/SILERO-VAD-MIT.txt" "$output_dir/licenses/SILERO-VAD-MIT.txt"

# Extract dependency-owned license files only after verify-locks.sh has
# authenticated the enclosing source archives. Keep each archive path visible
# in the output filename so the App license closure is auditable offline.
while IFS= read -r row; do
  component=$(jq -r '.id' <<<"$row")
  archive_name=$(jq -r '.file_name' <<<"$row")
  archive="$cache_dir/$archive_name"
  while IFS= read -r license_row; do
    archive_path=$(jq -r '.archive_path' <<<"$license_row")
    expected_size=$(jq -r '.size_bytes' <<<"$license_row")
    expected_sha=$(jq -r '.sha256' <<<"$license_row")
    [[ -n "$archive_path" && "$expected_size" != null && "$expected_sha" != null ]] || {
      echo "Incomplete locked license metadata: $component/$archive_path" >&2; exit 1;
    }
    license_name=$(basename "$archive_path")
    destination="$output_dir/licenses/${component}--${license_name}"
    tar -xOf "$archive" "$archive_path" > "$destination"
    [[ "$(stat -f '%z' "$destination")" == "$expected_size" ]] || { echo "License size mismatch: $archive_path" >&2; exit 1; }
    [[ "$(shasum -a 256 "$destination" | awk '{print $1}')" == "$expected_sha" ]] || { echo "License SHA-256 mismatch: $archive_path" >&2; exit 1; }
  done < <(jq -c '.license as $license | if ($license.files // null) != null then $license.files[] else {archive_path: $license.archive_path, size_bytes: $license.size_bytes, sha256: $license.sha256} end' <<<"$row")
done < <(jq -c '.source_archives[] | select(.license != null)' "$script_dir/source-lock.json")

jq -n '{files:[inputs]}' < <(
  find "$output_dir/licenses" -type f -not -name license-manifest.json -print | sort | while IFS= read -r license_file; do
    relative=${license_file#"$output_dir/"}
    jq -cn --arg path "$relative" \
      --argjson size "$(stat -f '%z' "$license_file")" \
      --arg sha256 "$(shasum -a 256 "$license_file" | awk '{print $1}')" \
      --arg mode "$(stat -f '%Lp' "$license_file")" \
      '{path:$path,size_bytes:$size,sha256:$sha256,mode:$mode}'
  done
) > "$output_dir/licenses/license-manifest.json"

manifest="$output_dir/.artifact-manifest.pending.json"
cp "$script_dir/artifact-manifest.macos-arm64.json" "$manifest"

while IFS= read -r row; do
  relative=$(jq -r '.path' <<<"$row")
  expected_size=$(jq -r '.size_bytes' <<<"$row")
  expected_sha=$(jq -r '.sha256' <<<"$row")
  path="$output_dir/$relative"
  [[ -f "$path" && ! -L "$path" ]] || { echo "Manifest file missing: $relative" >&2; exit 1; }
  actual_size=$(stat -f '%z' "$path")
  [[ "$actual_size" == "$expected_size" ]] || {
    echo "Manifest size mismatch: $relative (expected $expected_size bytes, got $actual_size)" >&2
    exit 1
  }
  [[ "$(shasum -a 256 "$path" | awk '{print $1}')" == "$expected_sha" ]] || { echo "Manifest SHA-256 mismatch: $relative" >&2; exit 1; }
done < <(jq -c '.files[]' "$manifest")

while IFS= read -r row; do
  relative=$(jq -r '.path' <<<"$row")
  license_file="$output_dir/$relative"
  [[ -f "$license_file" && ! -L "$license_file" ]] || { echo "Missing license file: $relative" >&2; exit 1; }
  [[ "$(stat -f '%z' "$license_file")" == "$(jq -r '.size_bytes' <<<"$row")" ]] || { echo "License size mismatch: $relative" >&2; exit 1; }
  [[ "$(shasum -a 256 "$license_file" | awk '{print $1}')" == "$(jq -r '.sha256' <<<"$row")" ]] || { echo "License SHA-256 mismatch: $relative" >&2; exit 1; }
  [[ "$(stat -f '%Lp' "$license_file")" == "$(jq -r '.mode' <<<"$row")" ]] || { echo "License mode mismatch: $relative" >&2; exit 1; }
done < <(jq -c '.files[]' "$output_dir/licenses/license-manifest.json")

while IFS= read -r license_file; do
  relative=${license_file#"$output_dir/"}
  jq -e --arg path "$relative" '[.files[].path] | index($path) != null' "$output_dir/licenses/license-manifest.json" >/dev/null || {
    echo "Unlisted license file: $relative" >&2; exit 1;
  }
done < <(find "$output_dir/licenses" -type f -not -name license-manifest.json -print)

while IFS= read -r path; do
  relative=${path#"$output_dir/"}
  case "$relative" in
    .artifact-manifest.pending.json|licenses/*) continue ;;
  esac
  jq -e --arg path "$relative" '[.files[].path] | index($path) != null' "$manifest" >/dev/null || {
    echo "Unlisted prepared artifact file: $relative" >&2; exit 1;
  }
done < <(find "$output_dir" -type f -not -path "$manifest" -print)

manifest_sha=$(shasum -a 256 "$manifest" | awk '{print $1}')
total_size=$(jq -er '[.files[].size_bytes] | add' "$manifest")
# Publish the completion marker only after every file/license has passed. A
# failed preparation must never be mistaken for a reusable verified artifact.
mv "$manifest" "$output_dir/artifact-manifest.json"
echo "Prepared Sherpa artifact: $output_dir"
echo "Manifest SHA-256: $manifest_sha"
echo "Artifact bytes: $total_size"
