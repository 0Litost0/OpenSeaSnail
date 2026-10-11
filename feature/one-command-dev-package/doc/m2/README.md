# M2 锁定输入与原生准备

2026-10-11，已完成并经子代理复审通过；本记录不代表公共 build/package 已完成。

统一适配位于 `scripts/dev_package/pipeline.py`。前端 frozen install、Cargo locked fetch、Swift force-resolved resolve 由阶段监督执行；OpenAPI 只向运行目录生成并比较，三份锁文件逐阶段比较摘要。工具路径固定为只读检查确认的现成二进制，未安装工具或修改全局配置。

下载使用锁定 HTTPS URL、大小和 SHA-256，连接 30 秒、总时长 3600 秒、120 秒低于 1 KiB/s 退出。暂态 curl/HTTP 状态最多三次尝试，完整性错误不重试、不覆盖已存在输入。Git 源码准备沿用既有 revision/submodule/clean 检查，并在自有工作区完成后发布；超时重试由父控制器在整个阶段进程组退出后执行，避免清理仍被 Git 后代使用的目录。Git 的低速配置仅通过命令环境设置；pnpm/Cargo 使用自身的有限重试及较长请求上限，依赖准备阶段另有整体期限。Cargo 的低速观察窗口与 HTTP timeout 共用，不能声称与 curl 的 120 秒窗口相同。

FFmpeg 的真实构建脚本已移到 phases，旧路径薄转发。部署目标读取 Info.plist，编译、链接和 host 参数显式传递；阶段隔离父 Git 发现。对应源码材料包含真实脚本和实际 configure 参数。新输出不借用原 canonical bundle，Sherpa 原有锁、manifest、模型及核心制品哈希保持。

原生构建目录按输入、工具二进制摘要、配置和脚本身份分开。只移动最终 runtime 制品，CMake 编译树留在原路径。完成凭据覆盖完整文件清单、链接、模式和摘要；变化或不完整结果拒绝复用。发布前持久化 pending receipt 和 staging 所有权，发布后中断可核对清单再补齐凭据；未发布 staging 重新构建。下载/源码/FFmpeg 工作区恢复只清理约定 bucket 内有匹配所有权标记的叶目录，未知内容保留并阻塞。恢复调用要求已持有仓库锁且残留进程组门禁通过。

目前证据保存在忽略目录 `dist/dev-package/m2-20261011/`：

- 第一轮准备运行 `20261011T085042630658Z-7406d836c983`：冻结依赖、严格输入校验、FFmpeg 和 Sherpa 冷构建阶段全部通过。原 private driver 的局部结果文件曾写为 succeeded，但其 broad BaseException 捕获又把 SystemExit(0) 误记为 failed/9；该 run 不能计为最终成功记录。requested_scope/validation_scope 均明确限制为 M2 preparation，不是公共 build 成功。初始局部结果保留于 preparation-initial-result.json；对应 reconstructed 脚本是按已执行版本重建的副本，不冒充原始执行前快照。后续采集器已改为 checked/prepared 并避免捕获正常退出，历史状态没有改写。
- `20261011T090816603960Z-4c8ca7df4627`：复验期间修正了源码循环提前退出问题，输入身份变化门禁返回 native_inputs_changed/5，未发布新原生制品。原始阶段日志和失败结果保留在对应 runs 目录。
- 修正后原生冷构建复验 `20261011T091124416552Z-f056373b7839` 的阶段通过，局部文件为 checked/0，但同样受旧采集器退出处理影响，run 最后为 failed/9。局部文件保存于 preparation-native-cold-result.json；最终成功记录须以修正采集器后的运行查询为准。
- 独立空源码实验 `20261011T110302271161Z-b73436ea9c59`：真实 cold sources/VAD 下载阶段退出 0（140.019 秒），损坏归档及脏 ONNX Runtime 源码被拒绝并保留。但后续原生复用触发事件字段 kind 与 Run.event 参数冲突，整个 run 未通过；已修复为 native_kind 并增加回归。后续真实准备运行 `20261011T110735524155Z-9079df1cceae` 已通过。
- tests-review-ready.log：55 项标准库测试通过，包含真实信号/超时、下载中断后同流程重跑、坏 hash 不重试、HTTP 有限重试、空源码目录同时准备两个 checkout、原生 rename 后凭据恢复、篡改拒绝及两个原生制品的复用事件。假下载与空源码 fixture 用于故障/控制流测试，不替代上游真实下载或原生构建。
- input-protection.json：原 630 项输入中只出现已授权的五项 M1/M2 适配变化；35 项旧原生制品摘要全部不变。

网络参数依据：[Cargo HTTP 配置](https://doc.rust-lang.org/stable/cargo/reference/config.html)、[pnpm 9.15.9 配置类型](https://github.com/pnpm/pnpm/blob/v9.15.9/config/config/src/types.ts)。实际命令及退出码以本轮阶段日志为准。

初审前实现验证记录：

- `20261011T110735524155Z-9079df1cceae`：修正采集器和复用事件后，真实准备为 checked/0。frontend/Cargo/Swift 阶段分别退出 0，锁摘要不变。FFmpeg 构建 32.576 秒、Sherpa 构建 399.783 秒、Sherpa prepare 109.925 秒；仅记录阶段实测，不承诺固定时长。FFmpeg SHA-256 为 ec6d4045b32fd04ff0b25c6c1455e1009ce3d3173f7cef8eb116213ff9b71b38、minos 13.0；Sherpa 三个核心文件满足原 pinned manifest。
- `20261011T111945223891Z-0664d6c7cb2a`：独立空源码、缺 VAD 实验为 checked/0。真实 clone/download 和完整验证通过；坏归档返回 5 且保留，脏 ONNX Runtime 源码返回 5 且保留。FFmpeg/Sherpa 均由完整凭据复用，没有再次编译，native_reused 事件准确记录。
- 真实 run 的 source/commands/process/state/result/events/build.log 留在对应 runs 目录；cold-inputs-result.json 包含实验边界，较大的既有锁定归档从当前 checkout 验证后复制，未声称本轮空全局缓存或全部模型重新下载。M0 的真实完整下载证据仍独立保存。

子代理 `review_m2` 初审发现并修复两项问题，复审确认无新的必要问题：

- FFmpeg 工作区原先在异常 finally 和 shell EXIT 中无条件删除。受管理模式现在由父控制器在阶段正常返回、整个进程组 inactive 后清理；中断、超时或残留状态保留自有目录，后续恢复先经过仓库锁与残留进程门禁。旧独立脚本入口保留原清理行为。
- Sherpa artifact/license manifest 的畸形 JSON、非对象、缺字段和无效文件行统一转成完整性 Failure(5)，保留实际路径。
- tests-review-fixed.log：57 项测试通过。新增真实 SIGTERM/超时实验保留 TERM-resistant 孙进程，验证目录未删除、下一次恢复被 Residual 阻止，确认进程组 inactive 后才恢复自有目录；manifest 两类格式损坏覆盖通过。另修正既有 supervisor 测试自身启动竞态，等待工具 ready 后才发信号。

修复后最终验证：

- `20261011T113348867932Z-e88fc3258991`：修复改变原生输入身份，因此重新执行冻结依赖、严格源码/归档/模型校验和真实冷构建，最终 checked/0。FFmpeg 构建 31.219 秒，源码检查通过，SHA-256 仍为 ec6d4045b32fd04ff0b25c6c1455e1009ce3d3173f7cef8eb116213ff9b71b38、minos 13.0。Sherpa 构建 423.286 秒、prepare 107.828 秒，三个核心文件满足原固定 manifest；未手工升级旧凭据。日志 preparation-review-fixed.log，最终结果 preparation-result.json。
- `20261011T114518779976Z-fe27be58ee6b`：checked/0，FFmpeg/Sherpa 当前完整凭据均复用，两个 native_reused 事件且没有执行编译阶段；Sherpa 原 manifest、文件及许可证再次检查。日志 reuse-review-fixed.log，结果 reuse-review-fixed-result.json。
- input-protection-review-fixed.json：630 项原始输入仅有五项已授权适配变化，35 项旧原生制品摘要全部不变。57 项测试与两项必要修复已获子代理复审通过。

M2 已关闭，按用户要求暂停；M3 未启动。
