#!/usr/bin/env python3
"""SeaSnail's offline-only FunASR HTTP sidecar.

This deliberately does not use FunASR's stock server: all component models are
provided as local directories below ``--models-root`` and no hub identifier is
accepted.  The endpoint is OpenAI verbose-JSON compatible for the Rust driver.
"""

import argparse
import gc
import logging
import os
import re
import tempfile
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import JSONResponse
from funasr import AutoModel


def clean_text(value: str) -> str:
    return re.sub(r"<\|[^|]*\|>", "", value or "").strip()


def model_paths(root: Path, extra: Path | None = None) -> dict[str, str | None]:
    """Resolve local model dirs, merging a builtin ``root`` with an ``extra`` root.

    asr/vad are required from ``root`` (raise if missing — sidecar cannot run
    without them). punc/spk are optional and resolved dual-root (ST-M2.3):
    prefer ``extra/<name>`` (downloaded to the user dir), fall back to
    ``root/<name>`` (bundled), else ``None`` — so the sidecar starts with the
    minimal resident set and punc can be lazy-mounted from either location.

    注：``transcribe`` 按请求重解析 punc 路径（M4，下载后运行中 sidecar 即时采纳），
    不消费此处缓存的 ``punc``/``spk`` 条目；保留解析仅为完整性/未来 spk 接线。
    """
    paths: dict[str, str | None] = {}
    for name in ("asr", "vad"):
        p = root / name
        if not p.is_dir():
            raise RuntimeError(f"missing required FunASR model directory: {name}")
        paths[name] = str(p)
    for name in ("punc", "spk"):
        resolved = None
        if extra is not None and (extra / name).is_dir():
            resolved = str(extra / name)
        elif (root / name).is_dir():
            resolved = str(root / name)
        paths[name] = resolved
    return paths


def resolve_device(requested: str) -> str:
    """Resolve the actual execution device without assuming MPS is usable.

    The bundle always includes an MPS-capable Torch build, but an individual
    macOS session can still have MPS unavailable (for example CI or a remote
    login session). Falling back to CPU preserves offline functionality and
    makes the selected device visible from ``/health`` instead of silently
    claiming acceleration.
    """
    if requested == "cpu":
        return "cpu"
    if requested not in {"mps", "auto"}:
        raise ValueError(f"unsupported device: {requested}")
    import torch
    return "mps" if torch.backends.mps.is_available() else "cpu"


def _ensure_punc(model, want: bool, punc_path: str | None, device: str) -> None:
    """Lazy-mount/unload punc on the resident AutoModel (M2.2 + F-Risk1, M2.4 degrade).

    - want & not loaded & path available → ``AutoModel.build_model`` the punc model,
      assign ``punc_model``/``punc_kwargs``, then ``_store_base_configs()`` (F-Risk1:
      re-snapshot base so ``_reset_runtime_configs`` at next generate keeps
      ``punc_kwargs`` instead of wiping it to the asr+vad-only baseline → punc crash).
      Mounted model stays resident across requests (not reloaded).
    - not want & loaded → ``punc_model=None`` + ``punc_kwargs={}`` + re-snapshot + gc.
    - want & not loaded & no path or build fails → degrade (punc stays off, native
      SenseVoice punctuation), log WARN; never raises (M2.4: 不崩不阻断转写).
    """
    loaded = getattr(model, "punc_model", None) is not None
    if want and not loaded:
        if not punc_path:
            logging.warning("punc requested but no punc model available; using native punctuation")
            return
        try:
            logging.info("lazy-mounting punc model")
            punc_kwargs = {
                "model": punc_path, "model_revision": "master",
                "device": device, "ncpu": 4,
            }
            punc_model, punc_kwargs = AutoModel.build_model(**punc_kwargs)
            model.punc_model = punc_model
            model.punc_kwargs = punc_kwargs
            model._store_base_configs()
        except Exception as e:  # noqa: BLE001 - 降级，不崩，绝不外抛（M2.4）
            logging.warning("punc lazy-mount failed; degrading to native punctuation: %s", e)
            model.punc_model = None
            model.punc_kwargs = {}
            # 重拍快照本身可能再抛（病态 OOM）——守护之，确保 _ensure_punc 绝不逃逸
            # （transcribe 未包 try/except _ensure_punc），违反 M2.4 "不阻断转写"。
            try:
                model._store_base_configs()
            except Exception as e2:  # noqa: BLE001
                logging.warning("punc degrade re-snapshot failed: %s", e2)
    elif not want and loaded:
        logging.info("unloading punc model")
        model.punc_model = None
        model.punc_kwargs = {}
        model._store_base_configs()
        gc.collect()


def create_app(models_root: Path, requested_device: str, extra_root: Path | None = None) -> FastAPI:
    paths = model_paths(models_root, extra_root)
    device = resolve_device(requested_device)

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        # 最小常驻集：仅 asr+vad（M2.1）。punc 懒挂载（M2.2，首条 punc=true 时）、
        # spk 后置——均不在启动期 eager 加载。`disable_update` 避免 FunASR 联网版本检查。
        app.state.model = AutoModel(
            model=paths["asr"], vad_model=paths["vad"],
            device=device, disable_update=True,
        )
        yield

    app = FastAPI(title="SeaSnail FunASR Sidecar", lifespan=lifespan)

    @app.get("/health")
    async def health():
        model = getattr(app.state, "model", None)
        punc_loaded = model is not None and getattr(model, "punc_model", None) is not None
        return {"status": "ok", "offline": True, "device": device, "punc_loaded": punc_loaded}

    @app.post("/v1/audio/transcriptions")
    async def transcribe(
        file: UploadFile = File(...),
        response_format: str = Form("verbose_json"),
        language: str | None = Form(None),
        punc: bool = Form(False),
        spk: bool = Form(False),
    ):
        if response_format not in {"json", "verbose_json", "text"}:
            raise HTTPException(400, "unsupported response_format")
        suffix = Path(file.filename or "audio.wav").suffix or ".wav"
        with tempfile.NamedTemporaryFile(delete=False, suffix=suffix) as temporary:
            temporary.write(await file.read())
            audio_path = temporary.name
        try:
            model = app.state.model
            # M2.2/M4：punc 懒挂载/卸载（首条 punc=true 挂载常驻，punc=false 卸载）。
            # 路径按请求重解析（不依赖启动期缓存的 paths）：下载完成后运行中 sidecar 即时
            # 采纳新下载的 punc，无需重启（避免重载 asr+vad 阻塞实时）。
            punc_path = None
            if extra_root is not None and (extra_root / "punc").is_dir():
                punc_path = str(extra_root / "punc")
            elif (models_root / "punc").is_dir():
                punc_path = str(models_root / "punc")
            _ensure_punc(model, punc, punc_path, device)
            # M2.1：spk_mode 显式重置（每次 generate 前，F-Risk4）；punc 在→punc_segment
            # 否则→vad_segment，不依赖 FunASR 的静默突变。
            model.spk_mode = (
                "punc_segment" if getattr(model, "punc_model", None) is not None else "vad_segment"
            )
            # SPK 后置：spk_model 未加载则不请求 spk（防 return_spk_res=True 无模型崩）。
            return_spk_res = spk and getattr(model, "spk_model", None) is not None
            result = model.generate(
                input=audio_path, batch_size_s=60, language=language,
                use_itn=True, return_spk_res=return_spk_res,
                # M2 review: SenseVoice 仅在 output_timestamp=True 时产 words/timestamp；
                # 该标志默认只在 spk_model 非空时自动置位（auto_model.py:865）。M2 后置
                # spk（spk_model=None）须显式置，否则 words 恒空——断 M4.1 词级时间戳。
                output_timestamp=True,
            )[0]
        finally:
            os.unlink(audio_path)

        text = clean_text(result.get("text", ""))
        if response_format == "text":
            return JSONResponse(text)
        if response_format == "json":
            return {"text": text}
        segments = []
        for sentence in result.get("sentence_info", []):
            item = {
                "start": float(sentence.get("start", 0)) / 1000,
                "end": float(sentence.get("end", 0)) / 1000,
                "text": clean_text(sentence.get("text") or sentence.get("sentence") or ""),
            }
            if sentence.get("spk") is not None:
                item["speaker"] = f"speaker_{sentence['spk']}"
            segments.append(item)
        if not segments and text:
            segments.append({"start": 0.0, "end": 0.0, "text": text})
        # M4.1：词/字级时间戳（FunASR 字级）。words 与 timestamp 等长、单位 ms；转秒。
        # 不等长/缺省/任一项异常 → 不发 words（Rust validate_words 兜底清空，composer 走句段降级）。
        words = []
        raw_words = result.get("words") or []
        raw_ts = result.get("timestamp") or []
        if raw_words and len(raw_words) == len(raw_ts):
            for w, ts in zip(raw_words, raw_ts):
                if (
                    isinstance(ts, (list, tuple))
                    and len(ts) == 2
                    and w
                    and isinstance(ts[0], (int, float))
                    and isinstance(ts[1], (int, float))
                ):
                    words.append({
                        "start": float(ts[0]) / 1000,
                        "end": float(ts[1]) / 1000,
                        "text": str(w),
                    })
            if len(words) != len(raw_words):
                words = []
        return {
            "task": "transcribe", "text": text, "segments": segments,
            "language": language or "auto", "words": words,
        }

    return app


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--device", default="mps", choices=("auto", "mps", "cpu"))
    parser.add_argument("--models-root", type=Path, required=True)
    parser.add_argument("--models-extra-root", type=Path, default=None)
    args = parser.parse_args()
    import uvicorn
    uvicorn.run(create_app(args.models_root, args.device, args.models_extra_root), host=args.host, port=args.port)


if __name__ == "__main__":
    main()
