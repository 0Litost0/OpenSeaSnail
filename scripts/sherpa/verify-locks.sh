#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 --cache DIR [--source SHERPA_ONNX_CHECKOUT] [--onnxruntime-source ONNXRUNTIME_CHECKOUT]" >&2
}

cache_dir=""
source_dir=""
onnxruntime_source_dir=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --cache)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      cache_dir=$2
      shift 2
      ;;
    --source)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      source_dir=$2
      shift 2
      ;;
    --onnxruntime-source)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      onnxruntime_source_dir=$2
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

[[ -n "$cache_dir" ]] || { usage; exit 2; }
[[ -d "$cache_dir" ]] || { echo "Cache directory not found: $cache_dir" >&2; exit 1; }

for command_name in git jq shasum stat tar; do
  command -v "$command_name" >/dev/null || {
    echo "Required command not found: $command_name" >&2
    exit 1
  }
done

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
source_lock="$script_dir/source-lock.json"
artifact_lock="$script_dir/artifact-lock.json"

jq -e '
  .schema_version == 1 and
  (.sherpa_onnx.revision | test("^[0-9a-f]{40}$")) and
  ([.source_archives[].file_name] | length == (unique | length)) and
  all(.source_archives[]; (.sha256 | test("^[0-9a-f]{64}$")) and .size_bytes > 0) and
  ([.onnxruntime_cmake_dependencies[].id] | length == (unique | length)) and
  ([.onnxruntime_cmake_dependencies[].file_name] | length == (unique | length)) and
  all(.onnxruntime_cmake_dependencies[];
    (.sha1 | test("^[0-9a-f]{40}$")) and
    (.sha256 | test("^[0-9a-f]{64}$")) and
    .size_bytes > 0 and
    (.url | startswith("https://")))
' "$source_lock" >/dev/null

jq -e '
  .schema_version == 1 and
  .catalog_id == "sensevoice-small-sherpa-int8" and
  ([.downloads[].id] | length == (unique | length)) and
  all(.downloads[]; (.sha256 | test("^[0-9a-f]{64}$")) and .size_bytes > 0) and
  all(.archive_entries[]; (.sha256 | test("^[0-9a-f]{64}$")) and .size_bytes > 0)
' "$artifact_lock" >/dev/null

verify_file() {
  local path=$1
  local expected_size=$2
  local expected_sha=$3
  local actual_size actual_sha

  [[ -f "$path" && ! -L "$path" ]] || {
    echo "Missing or non-regular locked file: $path" >&2
    return 1
  }
  actual_size=$(stat -f '%z' "$path")
  [[ "$actual_size" == "$expected_size" ]] || {
    echo "Size mismatch for $path: expected $expected_size, got $actual_size" >&2
    return 1
  }
  actual_sha=$(shasum -a 256 "$path" | awk '{print $1}')
  [[ "$actual_sha" == "$expected_sha" ]] || {
    echo "SHA-256 mismatch for $path: expected $expected_sha, got $actual_sha" >&2
    return 1
  }
  echo "verified $path"
}

while IFS= read -r row; do
  relative_path=$(jq -r '.path' <<<"$row")
  size_bytes=$(jq -r '.size_bytes' <<<"$row")
  sha256=$(jq -r '.sha256' <<<"$row")
  verify_file "$script_dir/$relative_path" "$size_bytes" "$sha256"
done < <(jq -c '.license_bundle_files[]' "$source_lock")

while IFS= read -r row; do
  file_name=$(jq -r '.file_name' <<<"$row")
  size_bytes=$(jq -r '.size_bytes' <<<"$row")
  sha256=$(jq -r '.sha256' <<<"$row")
  verify_file "$cache_dir/$file_name" "$size_bytes" "$sha256"
done < <(jq -c '.source_archives[]' "$source_lock")

while IFS= read -r row; do
  file_name=$(jq -r '.file_name' <<<"$row")
  size_bytes=$(jq -r '.size_bytes' <<<"$row")
  sha256=$(jq -r '.sha256' <<<"$row")
  verify_file "$cache_dir/$file_name" "$size_bytes" "$sha256"
done < <(jq -c '.onnxruntime_cmake_dependencies[]' "$source_lock")

while IFS= read -r row; do
  file_name=$(jq -r '.file_name' <<<"$row")
  size_bytes=$(jq -r '.size_bytes' <<<"$row")
  sha256=$(jq -r '.sha256' <<<"$row")
  verify_file "$cache_dir/$file_name" "$size_bytes" "$sha256"
done < <(jq -c '.downloads[]' "$artifact_lock")

while IFS= read -r row; do
  download_id=$(jq -r '.download_id' <<<"$row")
  archive_path=$(jq -r '.archive_path' <<<"$row")
  expected_size=$(jq -r '.size_bytes' <<<"$row")
  expected_sha=$(jq -r '.sha256' <<<"$row")
  archive_name=$(jq -r --arg id "$download_id" '.downloads[] | select(.id == $id) | .file_name' "$artifact_lock")
  [[ -n "$archive_name" ]] || { echo "Unknown download_id: $download_id" >&2; exit 1; }

  actual_size=$(tar -xOjf "$cache_dir/$archive_name" "$archive_path" | wc -c | tr -d '[:space:]')
  [[ "$actual_size" == "$expected_size" ]] || {
    echo "Archive entry size mismatch for $archive_path: expected $expected_size, got $actual_size" >&2
    exit 1
  }
  actual_sha=$(tar -xOjf "$cache_dir/$archive_name" "$archive_path" | shasum -a 256 | awk '{print $1}')
  [[ "$actual_sha" == "$expected_sha" ]] || {
    echo "Archive entry SHA-256 mismatch for $archive_path: expected $expected_sha, got $actual_sha" >&2
    exit 1
  }
  echo "verified $archive_name:$archive_path"
done < <(jq -c '.archive_entries[]' "$artifact_lock")

if [[ -n "$source_dir" ]]; then
  [[ -d "$source_dir/.git" ]] || { echo "Not a Git checkout: $source_dir" >&2; exit 1; }
  expected_revision=$(jq -r '.sherpa_onnx.revision' "$source_lock")
  actual_revision=$(git -C "$source_dir" rev-parse HEAD)
  [[ "$actual_revision" == "$expected_revision" ]] || {
    echo "Sherpa revision mismatch: expected $expected_revision, got $actual_revision" >&2
    exit 1
  }
  [[ -z "$(git -C "$source_dir" status --porcelain --untracked-files=normal)" ]] || {
    echo "Sherpa checkout is dirty: $source_dir" >&2
    exit 1
  }
  license_sha=$(jq -r '.sherpa_onnx.license.sha256' "$source_lock")
  license_size=$(jq -r '.sherpa_onnx.license.size_bytes' "$source_lock")
  verify_file "$source_dir/LICENSE" "$license_size" "$license_sha"
  echo "verified Sherpa source revision $actual_revision"
fi

if [[ -n "$onnxruntime_source_dir" ]]; then
  [[ -d "$onnxruntime_source_dir/.git" ]] || {
    echo "Not an ONNX Runtime Git checkout: $onnxruntime_source_dir" >&2
    exit 1
  }
  expected_revision=$(jq -r '.onnxruntime.revision' "$source_lock")
  actual_revision=$(git -C "$onnxruntime_source_dir" rev-parse HEAD)
  [[ "$actual_revision" == "$expected_revision" ]] || {
    echo "ONNX Runtime revision mismatch: expected $expected_revision, got $actual_revision" >&2
    exit 1
  }
  [[ -z "$(git -C "$onnxruntime_source_dir" status --porcelain --untracked-files=normal)" ]] || {
    echo "ONNX Runtime checkout is dirty: $onnxruntime_source_dir" >&2
    exit 1
  }

  while IFS= read -r row; do
    submodule_path=$(jq -r '.path' <<<"$row")
    expected_submodule_revision=$(jq -r '.revision' <<<"$row")
    [[ -d "$onnxruntime_source_dir/$submodule_path/.git" || -f "$onnxruntime_source_dir/$submodule_path/.git" ]] || {
      echo "ONNX Runtime submodule is not initialized: $submodule_path" >&2
      exit 1
    }
    actual_submodule_revision=$(git -C "$onnxruntime_source_dir/$submodule_path" rev-parse HEAD)
    [[ "$actual_submodule_revision" == "$expected_submodule_revision" ]] || {
      echo "ONNX Runtime submodule revision mismatch for $submodule_path: expected $expected_submodule_revision, got $actual_submodule_revision" >&2
      exit 1
    }
  done < <(jq -c '.onnxruntime.submodules[]' "$source_lock")

  dependency_manifest=$(jq -r '.onnxruntime.dependency_manifest.path' "$source_lock")
  dependency_manifest_size=$(jq -r '.onnxruntime.dependency_manifest.size_bytes' "$source_lock")
  dependency_manifest_sha=$(jq -r '.onnxruntime.dependency_manifest.sha256' "$source_lock")
  verify_file "$onnxruntime_source_dir/$dependency_manifest" "$dependency_manifest_size" "$dependency_manifest_sha"

  while IFS= read -r row; do
    dependency_id=$(jq -r '.id' <<<"$row")
    expected_url=$(jq -r '.url' <<<"$row")
    expected_sha1=$(jq -r '.sha1' <<<"$row")
    manifest_row=$(awk -F';' -v id="$dependency_id" '$1 == id { print; exit }' \
      "$onnxruntime_source_dir/$dependency_manifest")
    [[ -n "$manifest_row" ]] || {
      echo "ONNX Runtime dependency missing from deps.txt: $dependency_id" >&2
      exit 1
    }
    actual_url=$(cut -d';' -f2 <<<"$manifest_row")
    actual_sha1=$(cut -d';' -f3 <<<"$manifest_row")
    [[ "$actual_url" == "$expected_url" && "$actual_sha1" == "$expected_sha1" ]] || {
      echo "ONNX Runtime dependency manifest drift for $dependency_id" >&2
      exit 1
    }
  done < <(jq -c '.onnxruntime_cmake_dependencies[]' "$source_lock")

  onnx_license_path=$(jq -r '.onnxruntime.license.source_path' "$source_lock")
  onnx_license_size=$(jq -r '.onnxruntime.license.size_bytes' "$source_lock")
  onnx_license_sha=$(jq -r '.onnxruntime.license.sha256' "$source_lock")
  verify_file "$onnxruntime_source_dir/$onnx_license_path" "$onnx_license_size" "$onnx_license_sha"
  notices_path=$(jq -r '.onnxruntime.license.third_party_notices_path' "$source_lock")
  notices_size=$(jq -r '.onnxruntime.license.third_party_notices_size_bytes' "$source_lock")
  notices_sha=$(jq -r '.onnxruntime.license.third_party_notices_sha256' "$source_lock")
  verify_file "$onnxruntime_source_dir/$notices_path" "$notices_size" "$notices_sha"
  echo "verified ONNX Runtime source revision $actual_revision"
fi

echo "Sherpa source and artifact locks verified"
