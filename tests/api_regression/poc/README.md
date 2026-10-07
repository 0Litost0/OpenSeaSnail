# API 回归框架选型 PoC

本目录是经用户授权的选型实验，不是正式测试框架实现。参见 [验证计划](PLAN.md) 和 [结果与推荐](RESULTS.md)。没有改变生产代码、生产构建或个人 Keychain。

## 环境准备

本次实测：macOS arm64、Node 22.12.0、pnpm 9.15.9、Rust/Cargo 1.97.1；Playwright 1.63.0、Vitest 4.1.10、Vite 8.2.1、nextest 0.9.146。Node 与 Cargo 依赖分别锁定。需要允许子进程和本机 loopback HTTP 监听。

从仓库根执行：

```sh
cd tests/api_regression/poc
pnpm install --frozen-lockfile
cargo build --locked --manifest-path Cargo.toml --target-dir ../../../target --bin seasnail-poc-host
```

nextest 按[官方安装说明](https://nexte.st/docs/installation/pre-built-binaries/)准备，设置 `POC_NEXTEST` 为其绝对路径。本次独立下载到 `/private/tmp/seasnail-poc-tools/cargo-nextest`，未全局安装。

- 版本：0.9.146，commit `8af696ddcce8fff2962d6a5168b6d138b8616a35`。
- macOS universal 下载归档 SHA-256：`39785160b3c2f6ed9a765049cf4fa79f3b39aa02eb7598a5a0e2a1a0b9ffb9a8`。
- 不需要下载或启动浏览器。

`.npmrc` 禁止自动安装未使用的 peer，Vite 与仓库现有版本保持一致。初次安装时自动 peer 解析曾卡住，npm 10.9.0 的替代尝试出现 `edgesOut` 错误；收敛依赖后 pnpm 正常完成。这是安装环境记录，不作为框架业务能力排名依据。

## 第一阶段复现

```sh
python3 run-comparison.py
```

脚本保存每次命令、真实退出码、预期退出码、耗时和原始报告。故意失败与复现命令应非零退出；脚本本身仅在所有结果符合预期时返回 0。`POC_COMPARISON_DIR` 可改变证据目录，`POC_CANDIDATES=pw,vitest,rust` 可缩小候选范围。

单独运行、制造失败、撤销故障后重跑：

```sh
pnpm exec playwright test comparison.spec.ts --grep DICT-001
POC_FAULT=1 POC_OUTPUT=artifacts/manual-pw-failure pnpm exec playwright test comparison.spec.ts --grep DICT-001
pnpm exec vitest run comparison.test.ts -t DICT-001
POC_FAULT=1 POC_OUTPUT=artifacts/manual-vitest-failure pnpm exec vitest run annotations.test.ts --reporter=agent --reporter=json
cargo test --locked --manifest-path Cargo.toml --target-dir ../../../target --test comparison -- --nocapture
```

每次执行都创建新目录和账户。`POC_FAULT=1` 只改变 PoC 最后一步的预期以验证失败诊断，不修改产品。独立复现是重新构造失败条件，不依赖上次数据库。撤销该变量后的通过仅证明此受控断言故障已消除，不宣称修复了产品缺陷。

## 第二阶段复现

```sh
python3 run-validation.py
```

默认使用仓库 `dist/SeaSnail.app/Contents/Resources/asr` 与 `ffmpeg`。需先具备与当前产品契约匹配的制品；此脚本不下载模型，不使用个人数据。路径和身份记录见 `artifacts/validation/identity.json`。如果产品包不在默认位置，可以直接以绝对路径运行：

```sh
POC_RUNTIME=sherpa SEASNAIL_ASR_ROOT=/absolute/path/to/asr FFMPEG_PATH=/absolute/path/to/ffmpeg POC_OUTPUT=artifacts/manual-sherpa pnpm exec playwright test pipeline.spec.ts --grep SHERPA
POC_RUNTIME=sherpa SEASNAIL_ASR_ROOT=/absolute/path/to/asr FFMPEG_PATH=/absolute/path/to/ffmpeg POC_OUTPUT=artifacts/manual-vitest-sherpa pnpm exec vitest run pipeline.test.ts -t SHERPA
```

共享 `pipeline.mjs` 保证两个候选执行同一业务与断言；Playwright 使用其 APIRequestContext，Vitest 使用 Node fetch 的薄适配。PoC 的真实识别入口通过 `data/speech.json` 复用正式测试集中的 CC-BY-4.0 英文 FLEURS 样本；不再保留系统 TTS 音频。来源与署名见 [音频许可](../assets/AUDIO-LICENSE.md)。该入口不代表正式 ASR 质量验收。

其他诊断实验：

```sh
POC_FLAKY=1 POC_OUTPUT=artifacts/manual-pw-flaky pnpm exec playwright test comparison.spec.ts --retries=1
POC_OUTPUT=artifacts/manual-vitest-flaky pnpm exec vitest run retries.test.ts
POC_OUTPUT=artifacts/manual-vitest-strict pnpm exec vitest run retries.test.ts --reporter=default --reporter=json --reporter=./vitest/strict-retries.ts
POC_TIMEOUT=1 POC_OUTPUT=artifacts/manual-vitest-timeout pnpm exec vitest run lifecycle.test.ts
pnpm exec playwright test lifecycle.spec.ts
```

Playwright flaky、Vitest strict 与用例 timeout 命令预期退出 1；Vitest 原生 retry 命令预期退出 0，`meta.attempts` 保留先失败后通过，不能把这个原生结论当作完整验收通过。strict 示例另写 `acceptance.json` 并返回 1；正式框架仍需统一呈现原始测试结果与整体验收结论。

真实 runner 崩溃实验：以真实 Sherpa 的上述环境变量运行 `playwright test lifecycle.spec.ts --grep LIFE-001`。只杀本次 runner，检查宿主及其直属 sidecar 退出，外层实验控制器恢复清理临时目录。生产级运行注册表和通用恢复工具尚未实现。

## 报告与凭据检查

`artifacts/`、`node_modules/` 和本地 `target/` 不入版本控制。保留的汇总见 [evidence/summary.json](evidence/summary.json)。原始失败记录不覆盖；重新运行时使用新的 `POC_OUTPUT`。

```sh
python3 audit-evidence.py artifacts/validation
```

扫描声明的合成凭据标记与完整 SeaSnail Token 格式，并展开 JSON 附件与 HTML 内嵌 ZIP。该扫描不证明任意秘密都能自动脱敏。测试凭据由进程环境或随机值提供，不写在可被失败报告附带的源码中；不要直接断言或输出 credential-bearing 整体响应。

## 代码边界

- `rust/host.rs`：隔离测试宿主，生产 router/鉴权/加密存储，测试专用持久凭据；mock 或真实 Sherpa。
- `shared/environment.mjs`：Node 候选共用进程管理，stdin EOF 退出、重启、有限等待、证据和清理。
- `shared/scenario.mjs`、`shared/pipeline.mjs`：共用业务流程与预期，框架提供断言和步骤适配。
- `playwright/`、`vitest/`、`rust/comparison.rs`：候选框架入口。
- `run-*.py`：此次实验的证据采集脚本，不是拟议中的通用测试执行器。

宿主在新建账户前允许 reconcile 延后，真实 Sherpa 的模型就绪在 PoC 启动期等待。其入口不是生产 daemon 的完整复制，未验证发布包入口、真实系统 Keychain 和所有启动故障。Sherpa 子进程观察目前显式为 macOS 适配；Windows 未验证。

## 音频替换后的证据边界

`evidence/` 和 RESULTS 中保留的是旧 PoC 的历史结果，其源文件和音频哈希不代表当前版本。新的执行需重新采集证据，不能将历史成功结论套用到替换后的音频。
