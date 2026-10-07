#!/usr/bin/env python3
"""ST-M5.2 sidecar 集成测试（hermetic，mock `funasr.AutoModel`）。

五个被测行为：
1. `punc=true` 首条懒挂载：调 `AutoModel.build_model` + `_store_base_configs()`（F-Risk1）。
2. `punc=false` 卸载：`punc_model=None` + `_store_base_configs()` + GC。
3. 切换 sidecar 后 `punc=true` 自恢复：新 app 的 model 初无 punc，首条 punc=true 重挂。
4. `spk_mode` 每次 generate 前重置（F-Risk4）：punc 在→punc_segment，否则 vad_segment。
5. `use_itn=True` 产 ITN：generate 恒传 use_itn=True（与标点正交）。

不依赖真模型：在 `import sidecar` 前 monkeypatch `sys.modules["funasr"]` 为 fake，
使 sidecar 顶部 `from funasr import AutoModel` 拿到可探单元。fastapi TestClient（httpx）走真路由。

运行（用随包 bundle Python，含 fastapi/httpx；无需 pytest）：
    python3 scripts/funasr/test_sidecar.py
或：
    third_party/funasr/macos-arm64/bundle-*/python/bin/python3 scripts/funasr/test_sidecar.py
"""

from __future__ import annotations

import sys
import types
import unittest
from pathlib import Path
from unittest.mock import patch

# ── 在 import sidecar 前注入 fake funasr，使 sidecar 的 `from funasr import
#    AutoModel` 拿到 FakeAutoModel（hermetic：不加载真 1G 权重）。─────────────
_FAKE_GENERATE_RETURN = [{"text": "你好，现在是9点5分。", "words": ["你", "好"], "timestamp": [[0, 100], [100, 200]]}]


class FakeAutoModel:
    """假 AutoModel：记录 build_model/_store_base_configs 调用、可控 generate 返回。

    作为常驻实例（lifespan 建）时，punc_model/spk_mode/punc_kwargs 等属性被 sidecar
    直接读写，故用真实可写属性而非 MagicMock（避免 getattr 返回 MagicMock 子对象
    被误判为 truthy）。build_model 是类方法，返回 (punc_model, punc_kwargs) 二元。
    """

    # 类级调用记录（跨实例共享，便于断言 build_model 次数）。
    build_model_calls: list[dict] = []
    generate_calls: list[dict] = []

    def __init__(self, **kwargs):
        self.init_kwargs = dict(kwargs)
        # 常驻模型属性：punc 未挂载初值。
        self.punc_model = None
        self.punc_kwargs: dict = {}
        self.spk_model = None
        self.spk_mode = "vad_segment"
        self._store_calls = 0
        # _snapshot_kwargs 模拟 FunASR `_store_base_configs` 拍的基线：`_reset_runtime_configs`
        # 在每次 generate 开头把运行态 `punc_kwargs` 重置回此基线。挂载 punc 后须再调
        # `_store_base_configs()` 把含 punc 的 kwargs 拍进基线，否则 reset 把 punc_kwargs
        # 抹回空 → punc 推理读空 kwargs 崩（F-Risk1）。
        self._snapshot_kwargs: dict = {}
        self._generate_return = list(_FAKE_GENERATE_RETURN)

    def _store_base_configs(self):
        self._store_calls += 1
        # 拍快照：把当前 punc_kwargs 作为 reset 基线。
        self._snapshot_kwargs = dict(self.punc_kwargs)

    def generate(self, **kwargs):
        # 模拟 FunASR `_reset_runtime_configs`（auto_model.py:864 每次 generate 开头）：
        # 把 punc_kwargs 重置回 `_store_base_configs` 拍的基线。
        self.punc_kwargs = dict(self._snapshot_kwargs)
        # F-Risk1：punc 已挂载（punc_model 非 None）但基线 kwargs 被抹空（未重拍快照）
        # → punc 推理读空 kwargs 崩。
        if self.punc_model is not None and not self.punc_kwargs:
            raise RuntimeError("punc_kwargs empty after reset (F-Risk1: _store_base_configs not called after mount)")
        FakeAutoModel.generate_calls.append(dict(kwargs))
        return list(self._generate_return)

    @classmethod
    def build_model(cls, **kwargs):
        cls.build_model_calls.append(dict(kwargs))
        # 返回 (punc_model 实例, punc_kwargs)；punc_model 用 truthy 标记已挂载。
        punc_model = types.SimpleNamespace(_from_build=True)
        return punc_model, dict(kwargs)


def _install_fake_funasr() -> types.ModuleType:
    mod = types.ModuleType("funasr")
    mod.AutoModel = FakeAutoModel
    sys.modules["funasr"] = mod
    return mod


_install_fake_funasr()

# 现在 import sidecar 会拿到 fake funasr。
sys.path.insert(0, str(Path(__file__).resolve().parent))
import sidecar  # noqa: E402  须在注入 fake funasr 之后


def _make_app(tmp: Path, extra: Path | None = None) -> tuple:
    """建一个 sidecar FastAPI app + TestClient，准备最小 asr/vad 目录。

    目录用 exist_ok=True，支持同一 tmp 下重复建 app（模拟 sidecar 重启）。
    """
    root = tmp / "bundle"
    (root / "asr").mkdir(parents=True, exist_ok=True)
    (root / "vad").mkdir(parents=True, exist_ok=True)
    if extra is not None:
        (extra / "punc").mkdir(parents=True, exist_ok=True)
    from fastapi.testclient import TestClient

    app = sidecar.create_app(root, "cpu", extra)
    # 触发 lifespan 建 app.state.model（TestClient 进 context 即跑 lifespan）。
    client = TestClient(app)
    client.__enter__()
    return app, client


class EnsurePuncMountTests(unittest.TestCase):
    """行为 1：punc=true 首条懒挂载（build_model + _store_base_configs）。"""

    def setUp(self):
        FakeAutoModel.build_model_calls.clear()
        FakeAutoModel.generate_calls.clear()

    def test_first_punc_true_mounts_and_snapshots(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            app, client = _make_app(tmp, extra=tmp / "extra")
            try:
                model = app.state.model
                self.assertIsNone(model.punc_model, "启动期 punc 未挂载")
                self.assertEqual(model._store_calls, 0, "启动期未拍 punc 快照")
                # 首条 punc=true 转写。
                resp = client.post(
                    "/v1/audio/transcriptions",
                    files={"file": ("a.wav", b"fake", "audio/wav")},
                    data={"punc": "true", "response_format": "verbose_json"},
                )
                self.assertEqual(resp.status_code, 200, resp.text)
                self.assertEqual(len(FakeAutoModel.build_model_calls), 1, "首条触发一次 build_model")
                kw = FakeAutoModel.build_model_calls[0]
                self.assertIn("model", kw, "build_model 传 punc 路径")
                self.assertEqual(kw.get("model_revision"), "master")
                self.assertEqual(kw.get("device"), "cpu")
                # F-Risk1：挂载后立即 _store_base_configs 重拍快照。
                self.assertGreaterEqual(model._store_calls, 1, "挂载后调 _store_base_configs")
                self.assertIsNotNone(model.punc_model, "挂载后 punc_model 非空")
                # /health 反映 punc_loaded。
                self.assertTrue(client.get("/health").json()["punc_loaded"], "/health punc_loaded=true")
            finally:
                client.__exit__(None, None, None)

    def test_subsequent_punc_true_reuses_resident_no_rebuild(self):
        # 已挂载的 punc 跨请求常驻，不重载 1G（机制声明）。
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            app, client = _make_app(tmp, extra=tmp / "extra")
            try:
                for _ in range(3):
                    client.post(
                        "/v1/audio/transcriptions",
                        files={"file": ("a.wav", b"fake", "audio/wav")},
                        data={"punc": "true"},
                    )
                # 首条挂载一次，后续复用 → build_model 仅一次。
                self.assertEqual(len(FakeAutoModel.build_model_calls), 1, "常驻复用不重载")
            finally:
                client.__exit__(None, None, None)


class EnsurePuncUnloadTests(unittest.TestCase):
    """行为 2：punc=false 卸载（punc_model=None + 快照 + GC）。"""

    def setUp(self):
        FakeAutoModel.build_model_calls.clear()
        FakeAutoModel.generate_calls.clear()

    def test_punc_false_unloads_after_mounted(self):
        import gc

        gc.collect()
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            app, client = _make_app(tmp, extra=tmp / "extra")
            try:
                model = app.state.model
                # 先挂载。
                client.post(
                    "/v1/audio/transcriptions",
                    files={"file": ("a.wav", b"fake", "audio/wav")},
                    data={"punc": "true"},
                )
                self.assertIsNotNone(model.punc_model)
                store_after_mount = model._store_calls
                # 卸载（punc=false）。
                client.post(
                    "/v1/audio/transcriptions",
                    files={"file": ("a.wav", b"fake", "audio/wav")},
                    data={"punc": "false"},
                )
                self.assertIsNone(model.punc_model, "卸载后 punc_model=None")
                self.assertEqual(model.punc_kwargs, {}, "卸载后 punc_kwargs 清空")
                self.assertGreater(model._store_calls, store_after_mount, "卸载后再拍快照")
                self.assertFalse(client.get("/health").json()["punc_loaded"], "/health punc_loaded=false")
            finally:
                client.__exit__(None, None, None)


class EnsurePuncDegradeTests(unittest.TestCase):
    """M2.4 失败降级：挂载失败/无路径 → 降级原生标点，绝不外抛、不阻断转写。

    覆盖 `sidecar.py:86-108` 三个降级路径：
    (a) want 但无 punc_path → 原生标点降级（:86-88）；
    (b) build_model 抛异常 → catch 后 punc_model=None + 重拍快照（:99-102）；
    (c) degrade 内 _store_base_configs 再抛 → 守护不逃逸（:105-108）。
    """

    def setUp(self):
        FakeAutoModel.build_model_calls.clear()
        FakeAutoModel.generate_calls.clear()

    def test_no_punc_path_degrades_without_raising(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            # extra=None 且 root 无 punc → punc_path=None。
            app, client = _make_app(tmp, extra=None)
            try:
                # punc=true 但无路径：降级，不崩、不抛，转写仍完成。
                resp = client.post(
                    "/v1/audio/transcriptions",
                    files={"file": ("a.wav", b"fake", "audio/wav")},
                    data={"punc": "true"},
                )
                self.assertEqual(resp.status_code, 200, "降级不阻断转写")
                self.assertEqual(len(FakeAutoModel.build_model_calls), 0, "无路径不调 build_model")
                self.assertFalse(client.get("/health").json()["punc_loaded"], "降级后 punc 仍未挂载")
            finally:
                client.__exit__(None, None, None)

    def test_build_model_failure_degrades_and_not_raising(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            app, client = _make_app(tmp, extra=tmp / "extra")
            try:
                # 让 build_model 抛异常，模拟真模型加载失败。patch 自动还原。
                @classmethod
                def failing(cls, **kwargs):  # noqa: N805
                    cls.build_model_calls.append(dict(kwargs))
                    raise RuntimeError("simulated punc load failure")

                with patch.object(FakeAutoModel, "build_model", failing):
                    resp = client.post(
                        "/v1/audio/transcriptions",
                        files={"file": ("a.wav", b"fake", "audio/wav")},
                        data={"punc": "true"},
                    )
                self.assertEqual(resp.status_code, 200, "build 失败降级不阻断转写")
                model = app.state.model
                self.assertIsNone(model.punc_model, "build 失败后 punc_model=None")
                self.assertFalse(client.get("/health").json()["punc_loaded"])
            finally:
                client.__exit__(None, None, None)

    def test_degrade_inner_store_failure_does_not_escape(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            app, client = _make_app(tmp, extra=tmp / "extra")
            try:
                # build_model 失败 → 进 degrade except → _store_base_configs 再抛应被守护。
                @classmethod
                def failing(cls, **kwargs):  # noqa: N805
                    cls.build_model_calls.append(dict(kwargs))
                    raise RuntimeError("build fail")

                def poison_store(self):
                    raise RuntimeError("poisoned re-snapshot")

                with patch.object(FakeAutoModel, "build_model", failing), \
                     patch.object(FakeAutoModel, "_store_base_configs", poison_store):
                    resp = client.post(
                        "/v1/audio/transcriptions",
                        files={"file": ("a.wav", b"fake", "audio/wav")},
                        data={"punc": "true"},
                    )
                self.assertEqual(resp.status_code, 200, "degrade 内二次异常不逃逸、不阻断转写")
                self.assertIsNone(app.state.model.punc_model)
            finally:
                client.__exit__(None, None, None)


class SpkModeResetTests(unittest.TestCase):
    """行为 4：spk_mode 每次 generate 前重置（F-Risk4，不依赖静默突变）。"""

    def setUp(self):
        FakeAutoModel.build_model_calls.clear()
        FakeAutoModel.generate_calls.clear()

    def test_spk_mode_reset_per_request(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            app, client = _make_app(tmp, extra=tmp / "extra")
            try:
                model = app.state.model
                # 先污染 spk_mode（模拟上次调用残留/外部篡改）。
                model.spk_mode = "punc_segment"  # 伪残留

                # punc=false（无 punc）：应为 vad_segment。
                client.post(
                    "/v1/audio/transcriptions",
                    files={"file": ("a.wav", b"fake", "audio/wav")},
                    data={"punc": "false"},
                )
                self.assertEqual(model.spk_mode, "vad_segment", "无 punc → vad_segment")

                # punc=true（挂载 punc）：应为 punc_segment。
                client.post(
                    "/v1/audio/transcriptions",
                    files={"file": ("a.wav", b"fake", "audio/wav")},
                    data={"punc": "true"},
                )
                self.assertEqual(model.spk_mode, "punc_segment", "有 punc → punc_segment")

                # 再 punc=false：回到 vad_segment（每次重置，非保留）。
                client.post(
                    "/v1/audio/transcriptions",
                    files={"file": ("a.wav", b"fake", "audio/wav")},
                    data={"punc": "false"},
                )
                self.assertEqual(model.spk_mode, "vad_segment", "卸载后回 vad_segment")
            finally:
                client.__exit__(None, None, None)


class UseItnTests(unittest.TestCase):
    """行为 5：use_itn=True 恒传（与标点正交，取 ITN 数字格式化）。"""

    def setUp(self):
        FakeAutoModel.build_model_calls.clear()
        FakeAutoModel.generate_calls.clear()

    def test_generate_always_passes_use_itn_true(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            app, client = _make_app(tmp, extra=tmp / "extra")
            try:
                # 无论 punc 开关，generate 都应传 use_itn=True。
                client.post(
                    "/v1/audio/transcriptions",
                    files={"file": ("a.wav", b"fake", "audio/wav")},
                    data={"punc": "false"},
                )
                client.post(
                    "/v1/audio/transcriptions",
                    files={"file": ("a.wav", b"fake", "audio/wav")},
                    data={"punc": "true"},
                )
                self.assertGreaterEqual(len(FakeAutoModel.generate_calls), 2)
                for call in FakeAutoModel.generate_calls:
                    self.assertTrue(call.get("use_itn"), "generate 须传 use_itn=True")
                    self.assertTrue(call.get("output_timestamp"), "output_timestamp=True 保词级时间戳")
            finally:
                client.__exit__(None, None, None)

    def test_response_carries_text(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            app, client = _make_app(tmp, extra=tmp / "extra")
            try:
                resp = client.post(
                    "/v1/audio/transcriptions",
                    files={"file": ("a.wav", b"fake", "audio/wav")},
                    data={"punc": "false", "response_format": "verbose_json"},
                )
                body = resp.json()
                self.assertEqual(body["task"], "transcribe")
                self.assertIn("text", body)
                self.assertIn("segments", body)
                self.assertIn("words", body)
            finally:
                client.__exit__(None, None, None)


class SidecarRecoveryTests(unittest.TestCase):
    """行为 3：切换 sidecar 后 punc=true 自恢复（新 app 无 punc，首条重挂，无须回灌）。"""

    def setUp(self):
        FakeAutoModel.build_model_calls.clear()

    def test_new_app_recovers_punc_on_first_true(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            # 第一个 sidecar：挂载过 punc。
            app1, client1 = _make_app(tmp, extra=tmp / "extra")
            try:
                client1.post(
                    "/v1/audio/transcriptions",
                    files={"file": ("a.wav", b"fake", "audio/wav")},
                    data={"punc": "true"},
                )
                self.assertIsNotNone(app1.state.model.punc_model)
            finally:
                client1.__exit__(None, None, None)

            # 切换：新 sidecar（新 app 实例）——模拟 sidecar 重启。
            FakeAutoModel.build_model_calls.clear()
            app2, client2 = _make_app(tmp, extra=tmp / "extra")
            try:
                # 新 sidecar 启动期无 punc。
                self.assertIsNone(app2.state.model.punc_model, "新 sidecar 启动期无 punc")
                self.assertFalse(client2.get("/health").json()["punc_loaded"])
                # 首条 punc=true 自动重挂（懒，无须 daemon 回灌）。
                client2.post(
                    "/v1/audio/transcriptions",
                    files={"file": ("a.wav", b"fake", "audio/wav")},
                    data={"punc": "true"},
                )
                self.assertIsNotNone(app2.state.model.punc_model, "新 sidecar 首条 punc=true 自恢复")
                self.assertEqual(len(FakeAutoModel.build_model_calls), 1, "新 sidecar 重挂一次")
                self.assertTrue(client2.get("/health").json()["punc_loaded"])
            finally:
                client2.__exit__(None, None, None)


class ModelPathsTests(unittest.TestCase):
    """双根合并解析（M2.3，顺带覆盖：asr/vad 必需、punc 双根优先 extra 回落 root）。"""

    def test_asr_vad_required_missing_raises(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            root = tmp / "nobundle"  # 无 asr/vad
            with self.assertRaises(RuntimeError):
                sidecar.model_paths(root)

    def test_punc_prefers_extra_then_root_then_none(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            root = tmp / "r"
            (root / "asr").mkdir(parents=True)
            (root / "vad").mkdir(parents=True)
            (root / "punc").mkdir(parents=True)
            # 仅 root 有 punc → 取 root。
            p = sidecar.model_paths(root)
            self.assertEqual(p["asr"], str(root / "asr"))
            self.assertEqual(p["vad"], str(root / "vad"))
            self.assertEqual(p["punc"], str(root / "punc"))
            self.assertIsNone(p["spk"])
            # extra 有 punc → 优先 extra。
            extra = tmp / "extra"
            (extra / "punc").mkdir(parents=True)
            p2 = sidecar.model_paths(root, extra)
            self.assertEqual(p2["punc"], str(extra / "punc"), "优先 extra")
            # 无 punc 目录 → None。
            (extra / "punc").rmdir()
            (root / "punc").rmdir()
            p3 = sidecar.model_paths(root, extra)
            self.assertIsNone(p3["punc"])


class ResolveDeviceTests(unittest.TestCase):
    """M4.1/M5.3 回归：resolve_device 不退（cpu 直通 + 不支持设备 ValueError + auto/mps 回落 cpu）。

    不依赖真 torch：auto/mps 分支用 monkeypatch `torch.backends.mps.is_available` 控返回。
    """

    def test_cpu_passes_through(self):
        self.assertEqual(sidecar.resolve_device("cpu"), "cpu")

    def test_unsupported_device_raises(self):
        # 不依赖 torch，纯校验入参白名单。
        with self.assertRaises(ValueError):
            sidecar.resolve_device("cuda")
        with self.assertRaises(ValueError):
            sidecar.resolve_device("")

    def test_auto_falls_back_to_cpu_when_mps_unavailable(self):
        # 注入假 torch 模块（resolve_device 第 68 行 `import torch` 延迟）。
        fake_torch = types.SimpleNamespace()
        fake_backends = types.SimpleNamespace()
        fake_backends.mps = types.SimpleNamespace()
        fake_backends.mps.is_available = lambda: False
        fake_torch.backends = fake_backends
        with patch.dict(sys.modules, {"torch": fake_torch}):
            self.assertEqual(sidecar.resolve_device("auto"), "cpu", "MPS 不可用 → 回落 cpu")
            self.assertEqual(sidecar.resolve_device("mps"), "cpu")

    def test_auto_uses_mps_when_available(self):
        fake_torch = types.SimpleNamespace()
        fake_backends = types.SimpleNamespace()
        fake_backends.mps = types.SimpleNamespace()
        fake_backends.mps.is_available = lambda: True
        fake_torch.backends = fake_backends
        with patch.dict(sys.modules, {"torch": fake_torch}):
            self.assertEqual(sidecar.resolve_device("auto"), "mps")
            self.assertEqual(sidecar.resolve_device("mps"), "mps")


class WordsParsingTests(unittest.TestCase):
    """M4.1/M5.3 回归：sidecar words 解析降级逻辑不退（sidecar.py:200-219）。

    验证：words/timestamp 等长且形状合法 → 转 OpenAI words；不等长/畸形 → 清空。
    Rust 侧 `validate_words` 是兜底，Python 侧前置过滤也须守住。
    """

    def setUp(self):
        FakeAutoModel.build_model_calls.clear()
        FakeAutoModel.generate_calls.clear()

    def _transcribe(self, client):
        return client.post(
            "/v1/audio/transcriptions",
            files={"file": ("a.wav", b"fake", "audio/wav")},
            data={"punc": "false", "response_format": "verbose_json"},
        ).json()

    def test_aligned_words_pass_to_response(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            app, client = _make_app(tmp, extra=None)
            try:
                # fixture 给等长合法 words/timestamp → 应透传到响应 words。
                body = self._transcribe(client)
                self.assertEqual(len(body["words"]), 2, "等长合法 → 保留")
                self.assertEqual(body["words"][0]["text"], "你")
                self.assertAlmostEqual(body["words"][0]["start"], 0.0)
                self.assertAlmostEqual(body["words"][0]["end"], 0.1)
            finally:
                client.__exit__(None, None, None)

    def test_mismatched_length_clears_words(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            app, client = _make_app(tmp, extra=None)
            try:
                # 注入不等长：words 2 项、timestamp 3 项 → 清空。
                app.state.model._generate_return = [{
                    "text": "你好", "words": ["你", "好"],
                    "timestamp": [[0, 100], [100, 200], [200, 300]],
                }]
                body = self._transcribe(client)
                self.assertEqual(body["words"], [], "不等长 → 清空")
            finally:
                client.__exit__(None, None, None)

    def test_malformed_timestamp_clears_words(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            app, client = _make_app(tmp, extra=None)
            try:
                # 等长但 timestamp 元组形状非法（非二元/非数值）→ 整体清空。
                app.state.model._generate_return = [{
                    "text": "你好", "words": ["你", "好"],
                    "timestamp": [[0, 100], "bad"],
                }]
                body = self._transcribe(client)
                self.assertEqual(body["words"], [], "畸形 timestamp → 清空（非部分透传）")
            finally:
                client.__exit__(None, None, None)

    def test_missing_words_returns_empty(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            app, client = _make_app(tmp, extra=None)
            try:
                app.state.model._generate_return = [{"text": "无词级时间戳", "words": [], "timestamp": []}]
                body = self._transcribe(client)
                self.assertEqual(body["words"], [])
                self.assertIn("无词级时间戳", body["text"])
            finally:
                client.__exit__(None, None, None)


if __name__ == "__main__":
    unittest.main(verbosity=2)
