# 框架 PoC 历史结果与选型建议

> 本文是旧系统声音样本的历史实验记录。当前 PoC 已改用许可明确的 FLEURS 英文样本；以下识别输出、音频及源文件哈希不代表当前版本。当前测试素材见 [音频许可](../assets/AUDIO-LICENSE.md)。

> 2026-10-02。源码与可复现入口见 [README](README.md)，预先确定的比较标准见 [PLAN](PLAN.md)。这是选型证据，不是产品完整验收。

## 结论

**Playwright、Vitest、Rust/nextest 均可执行基础 API 业务回归。进一步实测表明，Vitest 用少量官方扩展即可补齐步骤诊断与严格重试门禁，并完成相同的 clean 和真实 Sherpa 链路。PoC 本身说明两种 Node 框架都可行；结合本需求对 Agent 步骤诊断、原生报告和 flaky 门禁的重视，当前已确认的技术设计方向采用 Playwright API 测试工程，保留 Rust 深层测试与既有前端 Vitest。**

PoC 曾基于工具链复用建议 Vitest，用户随后明确“复用成熟业界工具”优先于复用 SeaSnail 已有实现，并结合 Agent 使用效果选择 Playwright。该取舍没有改变实测数据：Vitest 可通过薄扩展实现相同核心功能，Playwright 的优势是步骤时间线、HTML 诊断与 flaky 门禁更原生。正式设计细节见 [API 回归设计](../docs/design.md)。

## 实测范围与证据

第一阶段执行用例发现、每候选 3 次独立成功、故意断言失败、干净环境独立复现、撤销故障后指定用例重跑；另外验证 Playwright 重试失败门禁。22 个命令结果均符合事先声明的退出码预期。

第一阶段最初使用各框架常规报告；补测 Vitest annotation/meta/Agent reporter 后，再比较实际适配成本。两个 Node 候选共用业务流程与环境管理。Rust 共用同一宿主与业务语义，但另写了 Rust 生命周期和 HTTP helper，其行数不能直接等同于 nextest 的固有成本。

| 能力 | Playwright 1.63.0 | Vitest 4.1.10 | Rust + nextest 0.9.146 |
| --- | --- | --- | --- |
| 真 HTTP、dictionary CRUD、真正进程重启 | 通过 | 通过 | 通过 |
| 单用例筛选、重复、独立复现、修复重跑 | 通过 | 通过 | 通过 |
| 预期/实际差异 | 原生断言与 JSON | 原生断言与 JSON | 原生断言与控制台/JUnit |
| 业务步骤结构化输出 | 原生 `test.step` | 小型 wrapper 写入 `meta.businessSteps`，annotation 展示步骤 | 本实验为 stderr 步骤记录；结构化步骤未实现 |
| 人类与机器报告 | 终端、JSON、HTML | 终端/Agent、JSON、JUnit | 终端、JUnit、用例列表 JSON |
| multipart、异步任务、二进制音频读取 | 通过，原生 HTTP 客户端 | 通过，Node fetch 薄适配 | 后续复杂场景未测试 |
| clean 成功、503、非法输出、真实超时回退 | 四项通过 | 同一四项通过 | 后续复杂场景未测试 |
| dictionary 进入 provider 请求、结果重启持久化 | 通过 | 通过 | 第一阶段仅 dictionary 持久化 |
| 真实 Sherpa、固定音频、内容断言、重启读取 | 通过 PoC 样例 | 同一 PoC 样例通过 | 未在该入口测试 |
| 缺模型制品 | 非零退出、说明启动错误 | 同样非零退出 | 未在该入口测试 |
| 用例超时后宿主退出与临时目录清理 | fixture teardown 通过 | AbortSignal + onTestFinished 通过 | 未测试超时门禁 |
| 首败后重试通过 | 原生 failOnFlakyTests 返回 1，保留两次结果 | 默认返回 0；官方 reporter 薄扩展后返回 1，meta 保留两次记录 | 本实验未测重试 |

“未测试”不是框架不支持。用例筛选产生的未执行项不得解释为整体验收通过。基线场景的少量执行耗时不足以做性能排名；测试环境、模型加载及进程启动远比框架本身影响大。

## 框架比较与设计取舍

- 仓库已有 Vitest 4.1.10 和 TS 工具链；API 工程可使用相同框架，但必须采用独立 Node 配置，不能沿用 jsdom/MSW 前端环境。
- 官方 annotation/meta 接口已经能承载步骤与生命周期；Agent reporter 可直接运行，适合终端失败诊断。
- 本次仅需两个小型适配：native fetch 客户端和严格重试 reporter；步骤记录也通过官方扩展接口完成。它们仍是 PoC，不能把当前代码量当作生产实现工作量估计。
- 两框架的核心隔离环境、凭据、模型制品、异常清理需求相同；Playwright 不会免除这些工作。
- 实测显示 Playwright 的原生 `test.step`、步骤耗时、fixture 与严格 flaky 开关更完整；Vitest 也可用薄扩展补足。结合本需求明确面向开发者与 Agent 的步骤诊断、报告即读和可靠门禁目标，技术设计选择 Playwright 作为主 API 回归框架。该选择依据目标使用体验与现成能力，而不是认为 Vitest 无法实现或必须复用仓库已有测试框架。

Rust/nextest 保留为产品内部测试的执行选择。本实验无需将已有 Rust 测试搬到 TypeScript，也没有证明 Rust 不适合 API 测试；只是当前业务步骤与 Agent 报告体验在 Playwright 中有更多原生支持。

## 发现并处理的问题

1. **共用宿主必须遵守生产启动语义。** 初版 PoC 把未解锁账户的 reconcile 错误当作致命错误，导致首次启动失败；已改为与生产一样允许延后。启动期阻塞 stdin watcher 也会延迟 Tokio runtime 退出，PoC 改用独立线程保持相同 EOF 语义。这是宿主适配问题，不能归咎于候选框架。
2. **收尾错误不能覆盖首次错误。** 初次真实模型启动失败被 cleanup 错误遮蔽；已保留启动错误并把清理错误作为额外证据。随后使用明确的制品绝对路径完成真实验证，原始失败仍保留在忽略的 artifacts 中。
3. **失败上下文也属于脱敏范围。** Playwright 即使关闭 trace，仍会附带错误位置周围源码。早期源码中的合成凭据标记出现在 error-context/HTML 附件；已改为运行时提供凭据。后续审计展开 HTML ZIP 与 JSON 附件后未发现声明的标记或完整 Token。不能据此宣称框架自动保护任意敏感数据。
4. **默认成功不一定满足验收规则。** Vitest 首次失败、第二次成功默认 exit 0；strict reporter 依据官方 flaky diagnostic 生成 `acceptance.json` 并返回 1。原生 JSON 仍表达测试最终成功，因此正式报告必须统一呈现原始结果与验收门禁，不让 Agent 只读其中一个。
5. **SIGKILL 不能依靠普通 finally 清目录。** 本次杀掉独立 runner 后，宿主通过 stdin EOF 退出；真实 Sherpa 直属 sidecar 也退出。外层实验控制器依据记录回收目录。正式方案仍需运行身份记录与恢复清理入口，不能把本次实验控制器当作已实现的通用恢复系统。
6. **报告采集也要校验。** nextest JUnit 位于 PoC workspace 的 `target/nextest`，并不随 Cargo `--target-dir` 移到仓库 target。早期采集脚本找错路径；已改为强制复制实际产物，并单独重跑 Rust 对比保留 XML。

## 真实模型结果的边界

固定音频参考：`Welcome to SeaSnail. This is a regression test. Please save this recording.`

两个 Node 候选均观察到：`Snail This is a regression test. Please save this recording.`

预先声明的两个短语 `regression test` 与 `save this recording` 均匹配，任务完成、结果持久化及真正重启读取通过。但开头有遗漏，因此本次只能证明真实转写与质量判断机制可用，**不能宣称完整文本准确，也不能充当正式质量门禁**。正式设计必须进一步确定代表性数据集、规范化、CER/WER 或关键内容规则及阈值。

制品由生产校验流程验证，音频 SHA-256 为 `df6b20f1ddd6dd5e4be28c95346ea8e5bd6c629c7c3094d385065a1a1d36742d`。具体构建、锁文件和 manifest 身份见保留的机器证据。

## 正式设计需要收敛的事项

- 以稳定用例 ID 为核心的覆盖矩阵、套件清单、数据与阈值版本。
- 共用生产启动组合逻辑的边界，测试凭据实现如何被限制在测试宿主。
- 将 PoC 的生命周期适配提升为有界、可恢复、不会干扰其他实例的环境管理。
- Playwright 主框架仍需项目专属的环境生命周期、凭据隔离、统一结果状态和脱敏适配；复用其原生步骤/报告/门禁能力，不建立通用执行器。
- API 调用、轮询、fixture 和清理的取消传播；统一“业务失败、环境错误、未执行、不稳定”的结论。
- 脱敏后的日志与附件策略、证据保留及运行时凭据来源。
- 产品 daemon 入口与测试宿主之间的一致性验证；当前未覆盖发行包 Keychain、安全签名、Windows、完整业务矩阵或真实付费 provider。

## 官方能力依据

- [Playwright API 测试](https://playwright.dev/docs/api-testing)、[fixture](https://playwright.dev/docs/test-fixtures)、[报告器](https://playwright.dev/docs/test-reporters)、[CLI](https://playwright.dev/docs/test-cli)。
- [Vitest 报告器](https://vitest.dev/guide/reporters)、[Test context](https://vitest.dev/guide/test-context)、[自定义报告器](https://vitest.dev/advanced/reporters)。
- [nextest JUnit](https://nexte.st/docs/machine-readable/junit/)、[官方二进制安装](https://nexte.st/docs/installation/pre-built-binaries/)。

在线文档用于候选调研；本次能力结论以锁定版本的源码、运行结果与报告为准。
