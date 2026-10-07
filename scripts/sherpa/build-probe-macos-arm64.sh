#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 --source DIR --onnxruntime-source DIR --cache DIR --output DIR" >&2
}

source_dir=""
onnxruntime_source_dir=""
cache_dir=""
output_dir=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --source) source_dir=${2:-}; shift 2 ;;
    --onnxruntime-source) onnxruntime_source_dir=${2:-}; shift 2 ;;
    --cache) cache_dir=${2:-}; shift 2 ;;
    --output) output_dir=${2:-}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

[[ -n "$source_dir" && -n "$onnxruntime_source_dir" && -n "$cache_dir" && -n "$output_dir" ]] || {
  usage
  exit 2
}
[[ -d "$source_dir/.git" ]] || { echo "Invalid Sherpa source: $source_dir" >&2; exit 1; }
[[ -d "$onnxruntime_source_dir/.git" ]] || {
  echo "Invalid ONNX Runtime source: $onnxruntime_source_dir" >&2
  exit 1
}
[[ -d "$cache_dir" ]] || { echo "Invalid cache: $cache_dir" >&2; exit 1; }

for command_name in cc c++ cmake file git install_name_tool jq lipo otool python3 shasum stat tar vtool; do
  command -v "$command_name" >/dev/null || {
    echo "Required command not found: $command_name" >&2
    exit 1
  }
done

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
python3 "$script_dir/reproducible-build.py" --check-toolchain
# __FILE__ is part of upstream error strings even in Release builds. Without
# prefix maps, changing the checkout directory changes native size and hashes.
mkdir -p "$output_dir"
source_dir=$(cd "$source_dir" && pwd -P)
onnxruntime_source_dir=$(cd "$onnxruntime_source_dir" && pwd -P)
output_dir=$(cd "$output_dir" && pwd -P)
prefix_flags=$(python3 "$script_dir/reproducible-build.py" \
  "$onnxruntime_source_dir" "$source_dir" "$output_dir" "$script_dir")
"$script_dir/verify-locks.sh" \
  --cache "$cache_dir" \
  --source "$source_dir" \
  --onnxruntime-source "$onnxruntime_source_dir"

deployment_target=$(jq -r '.onnxruntime.deployment_target' "$script_dir/source-lock.json")
onnxruntime_build_dir="$output_dir/build/onnxruntime"
onnxruntime_install_dir="$output_dir/build/onnxruntime-install"
onnxruntime_mirror_dir="$output_dir/build/onnxruntime-mirror"
sherpa_build_dir="$output_dir/build/sherpa"

mkdir -p "$sherpa_build_dir" "$onnxruntime_install_dir" "$onnxruntime_mirror_dir" "$output_dir/install/bin"
while IFS= read -r file_name; do
  cp "$cache_dir/$file_name" "$sherpa_build_dir/$file_name"
done < <(jq -r '.source_archives[].file_name' "$script_dir/source-lock.json")

while IFS= read -r row; do
  file_name=$(jq -r '.file_name' <<<"$row")
  dependency_url=$(jq -r '.url' <<<"$row")
  mirror_relative_path=${dependency_url#https://}
  mirror_path="$onnxruntime_mirror_dir/$mirror_relative_path"
  mkdir -p "$(dirname "$mirror_path")"
  cp "$cache_dir/$file_name" "$mirror_path"
done < <(jq -c '.onnxruntime_cmake_dependencies[]' "$script_dir/source-lock.json")

python3 "$onnxruntime_source_dir/tools/ci_build/build.py" \
  --build_dir "$onnxruntime_build_dir" \
  --config Release \
  --update \
  --build \
  --build_shared_lib \
  --compile_no_warning_as_error \
  --skip_submodule_sync \
  --skip_tests \
  --parallel \
  --osx_arch arm64 \
  --apple_sysroot macosx \
  --apple_deploy_target "$deployment_target" \
  --cmake_deps_mirror_dir "$onnxruntime_mirror_dir" \
  --cmake_extra_defines \
    "CMAKE_C_FLAGS=$prefix_flags" \
    "CMAKE_CXX_FLAGS=$prefix_flags" \
    onnxruntime_BUILD_UNIT_TESTS=OFF \
    CMAKE_EXPORT_NO_PACKAGE_REGISTRY=ON \
    "CMAKE_OSX_DEPLOYMENT_TARGET=$deployment_target" \
    "CMAKE_INSTALL_PREFIX=$onnxruntime_install_dir" \
  --target install

SHERPA_ONNXRUNTIME_INCLUDE_DIR="$onnxruntime_install_dir/include/onnxruntime" \
SHERPA_ONNXRUNTIME_LIB_DIR="$onnxruntime_install_dir/lib" \
cmake -S "$source_dir" -B "$sherpa_build_dir" \
  -DCMAKE_PROJECT_INCLUDE="$script_dir/reproducible-rpath.cmake" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX="$output_dir/install" \
  -DCMAKE_OSX_ARCHITECTURES=arm64 \
  -DCMAKE_OSX_DEPLOYMENT_TARGET="$deployment_target" \
  -DCMAKE_C_FLAGS="$prefix_flags -DEIGEN_MPL2_ONLY" \
  -DCMAKE_CXX_FLAGS="$prefix_flags -DEIGEN_MPL2_ONLY -DSHERPA_ONNX_DISABLE_COREML" \
  -DCMAKE_INSTALL_RPATH=@loader_path \
  -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON \
  -DBUILD_SHARED_LIBS=ON \
  -DSHERPA_ONNX_ENABLE_C_API=ON \
  -DSHERPA_ONNX_ENABLE_BINARY=OFF \
  -DSHERPA_ONNX_BUILD_C_API_EXAMPLES=OFF \
  -DSHERPA_ONNX_ENABLE_PYTHON=OFF \
  -DSHERPA_ONNX_ENABLE_TESTS=OFF \
  -DSHERPA_ONNX_ENABLE_PORTAUDIO=OFF \
  -DSHERPA_ONNX_ENABLE_WEBSOCKET=OFF \
  -DSHERPA_ONNX_ENABLE_TTS=OFF \
  -DSHERPA_ONNX_ENABLE_SPEAKER_DIARIZATION=OFF \
  -DSHERPA_ONNX_USE_PRE_INSTALLED_ONNXRUNTIME_IF_AVAILABLE=ON

cmake --build "$sherpa_build_dir" --target install --parallel

# Normalize the dylib IDs. RPATHs must already be relative at link time;
# deleting absolute paths afterward cannot repair layout/UUID reproducibility.
sherpa_dylib="$output_dir/install/lib/libsherpa-onnx-c-api.dylib"
ort_source="$onnxruntime_install_dir/lib/libonnxruntime.1.dylib"
ort_dylib="$output_dir/install/lib/libonnxruntime.1.dylib"
cp -L "$ort_source" "$ort_dylib"
install_name_tool -id @rpath/libonnxruntime.1.dylib "$ort_dylib"
install_name_tool -id @rpath/libsherpa-onnx-c-api.dylib "$sherpa_dylib"
actual_rpaths=$(otool -l "$sherpa_dylib" | awk '/cmd LC_RPATH/{getline; getline; print $2}')
[[ "$actual_rpaths" == "@loader_path" ]] || {
  echo "Sherpa must link with only @loader_path RPATH; got: $actual_rpaths" >&2
  exit 1
}

cc -std=c11 -O2 -Wall -Wextra -Werror \
  "-ffile-prefix-map=$script_dir=/seasnail/scripts" \
  "-ffile-prefix-map=$output_dir=/seasnail/build" \
  -mmacosx-version-min="$deployment_target" \
  -I"$output_dir/install/include" \
  "$script_dir/probe/sensevoice_probe.c" \
  -L"$output_dir/install/lib" -lsherpa-onnx-c-api \
  -Wl,-rpath,@executable_path/../lib \
  -o "$output_dir/install/bin/seasnail-sherpa-probe"

cc -std=c11 -O2 -Wall -Wextra -Werror \
  "-ffile-prefix-map=$script_dir=/seasnail/scripts" \
  "-ffile-prefix-map=$output_dir=/seasnail/build" \
  -mmacosx-version-min="$deployment_target" \
  -I"$output_dir/install/include" \
  "$script_dir/probe/vad_sensevoice_probe.c" \
  -L"$output_dir/install/lib" -lsherpa-onnx-c-api \
  -Wl,-rpath,@executable_path/../lib \
  -o "$output_dir/install/bin/seasnail-sherpa-vad-probe"

c++ -std=c++17 -O2 -Wall -Wextra -Werror \
  "-ffile-prefix-map=$script_dir=/seasnail/scripts" \
  "-ffile-prefix-map=$output_dir=/seasnail/build" \
  -mmacosx-version-min="$deployment_target" \
  -I"$output_dir/install/include" \
  "$script_dir/sidecar/main.cc" \
  -L"$output_dir/install/lib" -lsherpa-onnx-c-api \
  -Wl,-rpath,@executable_path/../lib \
  -o "$output_dir/install/bin/seasnail-sherpa-sidecar"

[[ "$(lipo -archs "$output_dir/install/bin/seasnail-sherpa-probe")" == "arm64" ]] || {
  echo "Probe is not arm64" >&2
  exit 1
}

while IFS= read -r macho_path; do
  [[ "$(lipo -archs "$macho_path")" == "arm64" ]] || {
    echo "Mach-O is not arm64: $macho_path" >&2
    exit 1
  }
  actual_minos=$(vtool -show-build "$macho_path" | awk '/minos/{print $2; exit}')
  [[ "$actual_minos" == "$deployment_target" ]] || {
    echo "Deployment target mismatch for $macho_path: expected $deployment_target, got $actual_minos" >&2
    exit 1
  }
done < <(find "$output_dir/install" -type f -perm -111 -print)

echo "Built $output_dir/install/bin/seasnail-sherpa-probe"
echo "Built $output_dir/install/bin/seasnail-sherpa-vad-probe"
echo "Built $output_dir/install/bin/seasnail-sherpa-sidecar"
