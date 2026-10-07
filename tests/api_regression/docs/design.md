# API regression design and coverage

## Execution boundaries

Playwright Test runs in API mode with a Rust test host that shares production
routing and application composition. Fixtures provide persistent synthetic
credentials, deterministic runtime scenarios and local provider behavior.
Rust tests continue to cover lower-level invariants; this project checks business
workflows, persistence, isolation, failure recovery and runner effectiveness.

The CLI validates registration and assets before execution, starts an isolated
host, collects business steps and attempts, confirms cleanup and publishes
sanitized JSON/HTML evidence. Real provider evaluation is explicitly opt-in.
See [requirements](requirements.md) and [commands](../README.md).

## Coverage matrix

The following 23 rows retain the existing case IDs, order, business promises and
quick/required membership. The independent required snapshot remains in
`contracts/acceptance-required.v1.json`; the executable catalog remains in
`case-catalog.json`. Contract checks compare them rather than deriving all
expectations from one source.

| Case ID | Setup / domain | Business promise | Membership |
| --- | --- | --- | --- |
| AUTH-001 | auth；空 home | status 未初始化 → setup 成功 → initialized；重复 setup 返回 410；无/错误 bearer 不能访问受保护 API | quick、必验 |
| AUTH-002 | auth；A/B 账户 | 错密码不切账户；A→B 时 A token 423；切回 A 后旧 token 可用 | quick、必验 |
| AUTH-003 | auth；A 与既有数据/token | 改密后旧密码 unlock 失败、新密码成功；未吊销 token 和历史数据保留 | quick、必验 |
| AUTH-004 | auth；A/B 账户 | 删除活跃账户被拒；切到 B 后删除 A，A 不再列出/解锁/访问，相关隔离存储清除（存储证据辅助） | quick、必验 |
| TOKEN-001 | auth；活跃账户 | 受限 token 可读允许资源，写/manage 被拒；不可授 scope 被拒；token 列表不返回 secret/hash | quick、必验 |
| TOKEN-002 | auth；有效受限 token | 吊销后所属活跃账户 API 返回 401；新 token 可用，拒绝不误作用于其他 token | quick、必验 |
| DICT-001 | dictionary；A | 增改删查、重复处理和无效输入符合契约；服务重启后原 token 读取一致 | quick、必验 |
| DICT-002 | dictionary；A/B 各有不同词条 | 列表/ID 操作不泄露或修改另一账户词条；realtime clean 请求包含当前账户词条且不混入另一账户词条 | quick、必验 |
| ASR-001 | sessions；确定性 ASR + 规范 WAV | multipart 提交、任务到 completed、固定转写与音频字节读取一致；列表/详情可查 | quick、必验 |
| ASR-002 | sessions；确定性 ASR 失败/修复配置 | 任务进入失败终态，错误可观察；经现有 retry API 恢复后结果正确，删除后列表/详情不可见 | quick、必验 |
| CLEAN-001 | clean；realtime + enabled + 成功 provider | 原文、cleaned/final text 和 cleanup 状态正确；词典参与；正常重启后可读取相同结果 | quick、必验 |
| CLEAN-002 | clean；realtime + HTTP 503 | 上游失败可观察、最终文本回退为原文；失败信息和结果持久化 | quick、必验 |
| CLEAN-003 | clean；realtime + 非法输出 | HTTP 成功但非法内容不能成为最终文本；按契约失败/回退并保留原文 | quick、必验 |
| CLEAN-004 | clean；realtime + 超时 provider | 真实超时路径有界进入约定回退，不无限挂起；错误 code 和持久化正确 | quick、必验 |
| CLEAN-005 | clean；分别关闭 enabled、使用 imported | 两种条件均不调用 provider，final text 为原文，详情分别体现关闭/未请求语义 | quick、必验 |
| RECOVERY-001 | lifecycle；持久化 Keychain、业务数据与 provider 凭据 | 真正停旧进程再启动；不先 unlock，原 token 可用、账户活跃、数据和 provider 认证可用 | quick、必验 |
| RECOVERY-002 | lifecycle；移除 MasterDek/启动时读失败 | 服务存活但原 token 423；密码 unlock 后数据恢复，不能重新 setup | quick、必验 |
| SHERPA-001 | sherpa；正式样本/制品/质量规则 | 真实 runtime 与模型身份正确；任务完成、每样本内容指标达标；正常重启后转写一致 | 必验；不在 quick |
| SYSTEM-001 | framework；代表性 auth/clean/dictionary 用例 | 新环境单跑、重复 3 次和组合执行的判断一致，无顺序依赖 | 必验；效果验收 |
| SYSTEM-002 | framework；独立子运行控制器 | 模拟缺制品、超时、runner 强杀、首败重试成功、筛选缺项和凭据 canary；子运行 gate 非通过、原因正确，资源回收/安全发布成立 | 必验；效果验收 |
| SYSTEM-003 | framework；版本化受控业务故障 | 原业务断言检测错误；新环境 replay 同样失败；撤销故障后该用例和相关套件通过 | 必验；效果验收 |
| SYSTEM-004 | framework；新增代表性用例示例 | 仅新增用例/数据/规则并登记，即可独立执行和纳入套件；无需修改通用执行器 | 必验；维护验收 |
| PROVIDER-001 | provider-eval；显式配置真实 provider/model | 记录输入、配置、输出与质量结果；无凭据时说明未执行，不计通过 | 可选入口 |

## Outcomes and replay

`passed` requires executed assertions and successful teardown. Business or
cleanup failures and flaky retries fail; environment/configuration errors,
interruption, missing required cases and unknown teardown produce incomplete
results. Filtering a required case does not turn a subset into full acceptance.

Replay binds source/build, assets, runtime, provider, credentials by reference,
quality/budget versions and any fault configuration. Credentials themselves
must not enter the configuration or report. Reports acquired before this source
snapshot should be kept as historical evidence, not replayed as current identity.

## Quality and measurements

[Licensed fixture decision](quality-approval.v3.md) preserves the prior numeric
CER/WER limits and normalization on a new, independently versioned FLEURS corpus.
Business-only audio is procedural. The old system-voice baseline is not reused.
New formal-host observations belong to `baseline-observations.fleurs-v1.json`;
re-scoring is a consistency check, not a new ASR run. New local measurement and
analysis output stays under ignored `artifacts/baseline/`.
