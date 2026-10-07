# API 回归执行项目

本项目在 macOS 使用 Playwright Test 的纯 API 模式，不安装浏览器。生产业务入口来自共享 Rust daemon 组合；测试 host、持久化合成 Keychain 和确定性 runtime 只存在于独立测试目标。

开发者和 Agent 的测试选择、失败处理及交付要求统一见[开发指南](../../doc/development.zh-CN.md)。本文说明具体操作；所有命令均从仓库根目录执行。

## 环境准备与首次运行

需要项目开发环境中的 Node.js / pnpm、Rust 工具链与 Cargo 依赖，以及 Python 3.10+ 和固定版本的 `jsonschema`（契约检查使用）。可先用 `python3 -c "import jsonschema"` 检查 Python 依赖。测试无需安装 Playwright 浏览器；宿主启动、原生进程身份核验和 loopback 通信需要本机运行权限。

```sh
# 在项目 Python 环境中安装契约校验依赖
python3 -m pip install -r tests/api_regression/contracts/requirements.txt
pnpm --dir tests/api_regression install --frozen-lockfile
pnpm --dir tests/api_regression build
pnpm --dir tests/api_regression api-test list
pnpm --dir tests/api_regression api-test run CLEAN-002
pnpm --dir tests/api_regression api-test suite clean
pnpm --dir tests/api_regression api-test quick
pnpm --dir tests/api_regression api-test acceptance
pnpm --dir tests/api_regression api-test cleanup-stale
```

## 选择执行范围

| 目标 | 命令（前缀均为 `pnpm --dir tests/api_regression api-test`） |
| --- | --- |
| 查看登记 | `list`；机器可读输出使用 `list --json` |
| 定位业务改动 | `run CLEAN-002`，替换为目标 case ID |
| 验证同一业务域 | `suite clean`，其他 suite 名称参见用例登记 |
| 日常快速回归 | `quick`（17 项） |
| 完整验收 | `acceptance`（22 项必验） |
| 回收中断后遗留实例 | `cleanup-stale` |

默认单 worker 串行执行，重试次数为 0。显式设置 `API_TEST_RETRIES` 后，首次失败和后续成功仍会作为 flaky 保留，并被门禁拒绝。通过 CLI 执行业务回归，才能得到登记校验、隔离、报告发布与退出码聚合；直接运行下方 Playwright 基础设施自检只验证对应检查。

新增用例从[扩展模板与登记指南](examples/README.md)开始。独立扩展需在 list/run/suite 时显式传入同一个 `--extension`；不会自动进入默认 quick/acceptance。

## 可选 provider 评测

可选真实 provider 评测（不属于 quick/acceptance，未显式选择时不产生任何远端调用）：

```sh
export SEASNAIL_API_TEST_EVAL_ENDPOINT=<provider endpoint>
export SEASNAIL_API_TEST_EVAL_CREDENTIAL=<provider credential>
pnpm --dir tests/api_regression api-test provider-eval --provider fixture-local-openai
```

provider 条目登记于 `assets/provider-eval.v1.json`（值只含环境变量名）；真实 provider 条目写入不提交的 `assets/provider-eval.local.json`，endpoint 与凭据始终运行时从所指环境变量解析。缺凭据或用 `run PROVIDER-001` 直接运行为 not_run/exit 2，不计通过；评测记录（输入/配置/输出/质量观察）随报告发布于 `environment-proof/PROVIDER-001-eval-<retry>.json`（仅含 endpoint_ref 与其 SHA-256，不含解析值），远端输出标注非确定，不构成质量门禁。

需 Rust 工具链及 Cargo 缓存/依赖；CLI 每次执行前进行 offline/locked 增量 host 构建，缺依赖退出 2。API、原生进程身份核验及 loopback 端口需本机运行权限。登记的 23 个用例均已有实现：17 quick、22 必验及可选 PROVIDER-001；完整验收以当次全部必验实际通过为准。M1 quality-v3 和预算已获用户批准；只有英文品牌词暂非强制，缺失仍记录。

真实 Sherpa 需要 M1 固定制品及 FFmpeg。默认从仓库 `dist/SeaSnail.app/Contents/Resources/asr` 和 `.../ffmpeg` 读取，也可设置 `SEASNAIL_ASR_ROOT` / `FFMPEG_PATH`。`assets/sherpa-baseline.v2.json` 固定完整 bundle 与 FFmpeg SHA；缺文件/不同制品明确 incomplete，不能回退确定性 runtime。SHERPA-001 在正式宿主执行中英文各三次真实转写，按批准规则评分，并验证正常重启和回收。当前验收范围是 macOS arm64；Linux/Windows 未实机验收，合成 Keychain 不代表系统 Keychain 安全性，付费真实 provider 输出质量未作为必验条件。

## 报告、诊断与退出码

CLI stdout 是安全 JSON 摘要。成功检查后生成 `artifacts/reports/<run_id>/result.json`、`html/index.html`、`replay/<case_id>.json`，在浏览器打开 HTML 即可查看。报告保存全部 attempts、步骤、业务/清理错误和构建/资产身份；`environment-proof` 记录实际宿主身份、配置、home 摘要及回收状态；`<case>-<retry>-diagnostics.json` 保留停止后采集的脱敏日志、退出状态和原生错误，结构化错误关联断言差异与证据。退出码 0 为通过，1 为业务/清理/报告失败，2 为缺项、配置/环境不足或中断；并存按 2 > 1 > 0 聚合，重试成功仍保留首次失败。

遇到失败时，先查看 `result.json` 的执行范围、attempts、错误分类和 teardown，再沿证据引用查看步骤及 diagnostics。缺环境或制品属于 incomplete，业务断言失败与回收失败也分别保留；不要仅凭 HTML 中单个绿色用例判断整体验收。向用户交付时按根 README 报告命令、run_id、退出码、实际范围和报告路径。

## 精确复现与修复后重跑

```sh
pnpm --dir tests/api_regression api-test replay /absolute/path/replay/CLEAN-002.json
pnpm --dir tests/api_regression api-test run CLEAN-002 --config /absolute/path/replay/CLEAN-002.json
```

`replay` 要求原构建、场景、版本和资产 SHA 相同，始终使用新 run/home；不匹配明确拒绝。`run --config` 用当前构建执行相同条件，并保存原/当前构建比较。dirty 源码快照位于受限本地 `artifacts/restricted/source-assets/<sha>.json`，不能随可发布报告传播；原资产缺失不能精确 replay。当前不自动恢复普通历史构建或注入远端 replay 凭据；M6 故障构建通过 hash 校验的 base_source 与版本化 patch 在隔离 source/target 重建，关闭增量编译并核验 binary SHA。replay 同时校验执行器、fixture、spec 与发布/契约代码的资产 SHA，执行代码变化时明确拒绝精确复现。

## 报告安全与遗留清理

原始原生报告/合成凭据位于受限目录，检查 JSON、HTML、内嵌 ZIP、附件和源码上下文后原子发布。解析/扫描/大小/时间检查失败只输出安全错误并销毁 raw；正常报告按 7 天在下次执行时清理。runner 强杀留下的实例由 `cleanup-stale` 按原生进程 birth identity 核验后回收，未知身份保持 recovery manifest，返回非零。不按名称或端口杀进程。

## 基础设施自检

按改动选择相关检查；基础设施自检不抵扣业务用例：

```sh
pnpm --dir tests/api_regression typecheck
pnpm --dir tests/api_regression test:environment
pnpm --dir tests/api_regression exec playwright test --config checks.config.ts
node --test tests/api_regression/scripts/test_results.mjs tests/api_regression/scripts/test_retention.mjs
python3 tests/api_regression/scripts/test_publisher.py
node tests/api_regression/scripts/test_report_security.mjs
node tests/api_regression/scripts/test_replay_cli.mjs
node tests/api_regression/scripts/test_provider_eval.mjs
python3 tests/api_regression/scripts/check_contracts.py all
python3 tests/api_regression/scripts/check_assets.py
```

稳定 case ID、required 快照和登记/发现关系独立校验。可透传 `-- --grep ...` 等 Playwright 参数；过滤、skip、缺结果和分片都不能被视为完整验收。

目录约定：实现位于 `tests/api_regression/`；当前需求、设计矩阵与质量依据位于 [docs](docs/design.md)。新的测量与分析输出写入已忽略的 `artifacts/baseline/`。迁移前的报告属于历史构建，精确 replay 会因构建/登记资产变化拒绝，应创建当前运行配置后再验证。


## M6 效果验收与故障演示

M6 效果验收：

```sh
pnpm --dir tests/api_regression api-test run SYSTEM-001
pnpm --dir tests/api_regression api-test run SYSTEM-003
pnpm --dir tests/api_regression api-test run SYSTEM-002
pnpm --dir tests/api_regression api-test run SYSTEM-004
pnpm --dir tests/api_regression api-test run CLEAN-002 --fault MUT-CLEAN-FALLBACK-001
pnpm --dir tests/api_regression api-test run DICT-001 --fault MUT-DICT-PERSIST-001
```

故障命令预期业务失败/exit 1；SYSTEM-003 通过表示它确认了原断言拒绝、第二个新环境的精确 replay 拒绝，以及撤销故障后的目标/相关回归通过。父报告在 `environment-proof/SYSTEM-003-<retry>/` 保留子运行的原始结论和证据副本，不将子失败反转成业务通过。SYSTEM-001 核验三类代表用例各三次独立运行及完整 quick 组合。控制器时限登记在 `assets/system-budgets.v1.json`；M1 的质量规则/预算批准状态保持不变。

边界自检（仓库根目录）：

```sh
node --test tests/api_regression/scripts/test_results.mjs tests/api_regression/scripts/test_diagnostics.mjs tests/api_regression/scripts/test_mutation.mjs
```

SYSTEM-002 核验环境不足、业务超时、首次失败后的 flaky、必验筛选缺项、安全发布拒绝以及真实 sidecar 存活时强杀 runner 后的 stale 回收。原 interrupted 子运行保持 exit 2 / unknown teardown，后续恢复清理单独记录；原失败不被控制器覆盖。SYSTEM-004 复制新增用例模板，核验发现/单跑/组合与默认 22 必验不变。新增用例和版本化资产维护见 [示例指南](examples/README.md)，通过显式 `--extension` 使用独立示例范围。
