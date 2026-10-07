#!/usr/bin/env python3
"""Download SeaSnail's default FunASR models into the repository-local cache.

Run this with the bootstrap interpreter prepared by download-wheels-macos-arm64.sh
(which has modelscope installed), never with a user's system interpreter:
  third_party/funasr/macos-arm64/cache/python-bootstrap/python/bin/python3 \
    scripts/funasr/download-models-macos-arm64.py
"""

import argparse
import hashlib
import json
import time
from pathlib import Path

from modelscope import snapshot_download

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_ROOT = ROOT / "third_party/funasr/macos-arm64/cache/models"

MODELS = {
    "asr": "iic/SenseVoiceSmall",
    "vad": "iic/speech_fsmn_vad_zh-cn-16k-common-pytorch",
    "punc": "iic/punc_ct-transformer_cn-en-common-vocab471067-large",
    "spk": "iic/speech_campplus_sv_zh-cn_16k-common",
}

REQUIRED_FILES = {
    "asr": "model.pt",
    "vad": "model.pt",
    "punc": "model.pt",
    "spk": "campplus_cn_common.bin",
}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def file_manifest(root: Path) -> list[dict[str, object]]:
    return [
        {"path": str(path.relative_to(root)), "size_bytes": path.stat().st_size, "sha256": sha256(path)}
        for path in sorted(root.rglob("*"))
        if path.is_file() and ".git" not in path.parts
    ]


def is_complete(role: str, destination: Path) -> bool:
    return (
        (destination / REQUIRED_FILES[role]).is_file()
        and not any(destination.rglob("*.incomplete"))
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=DEFAULT_ROOT)
    parser.add_argument("--revision", default="master", help="upstream snapshot revision (default: master)")
    parser.add_argument("--download-retries", type=int, default=6, help="retry a broken model transfer")
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=True)
    records = []
    for role, model_id in MODELS.items():
        destination = root / role
        # ModelScope leaves `*.incomplete` while a large LFS object is still
        # being fetched. Re-run safely resumes that one component; a complete
        # component is left untouched so an interrupted multi-model run is
        # recoverable without deleting user data.
        if not is_complete(role, destination):
            last_error = None
            for attempt in range(1, args.download_retries + 1):
                try:
                    # Serial downloads are slower but much more reliable on
                    # unstable connections, especially for ModelScope LFS
                    # artifacts. Existing `*.incomplete` files are resumed.
                    snapshot_download(
                        model_id=model_id,
                        revision=args.revision,
                        local_dir=str(destination),
                        max_workers=1,
                    )
                    last_error = None
                    break
                except Exception as error:  # ModelScope surfaces transport-specific types.
                    last_error = error
                    if attempt == args.download_retries:
                        break
                    delay = min(60, 2 ** attempt)
                    print(
                        f"{role}: download attempt {attempt}/{args.download_retries} failed: {error}; "
                        f"retrying in {delay}s",
                        flush=True,
                    )
                    time.sleep(delay)
            if last_error is not None:
                raise SystemExit(
                    f"Could not finish {role} after {args.download_retries} attempts. "
                    f"Keep the cache directory and rerun to resume: {last_error}"
                )
        if not is_complete(role, destination):
            partial = [str(p.relative_to(destination)) for p in destination.rglob("*.incomplete")]
            raise SystemExit(
                f"Model download incomplete for {role}: expected {REQUIRED_FILES[role]}; "
                f"partial files: {partial or 'none'}"
            )
        records.append({
            "role": role,
            "model_id": model_id,
            "revision": args.revision,
            "files": file_manifest(destination),
        })
    (root / "models-manifest.json").write_text(
        json.dumps({"format": 1, "models": records}, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    print(f"Downloaded {len(records)} model components to {root}")
    print(f"Manifest: {root / 'models-manifest.json'}")


if __name__ == "__main__":
    main()
