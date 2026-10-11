# M0 先行实验记录（2026-10-11）

用户授权范围为 M0 验证，不包含 M1～M7 的统一工具实现。这里的 Python 文件是一次性、可复核的实验设施，不是公开开发入口，也不构成已经实现的运行协议。

原始输出位于仓库内被忽略的 `dist/dev-package/m0-20261011/`。每次实验保留独立 `<name>.log` 与 `<name>.json`（命令、退出码、耗时、磁盘采样）；数据明细使用另外的文件名。失败尝试保留，不用后续成功覆盖。关键摘要与证据哈希保存在本目录的 `evidence-summary.json`。

当前状态：ST-M0.1～ST-M0.4 的关键实验完成；ST-M0.3 已完成冷输入获取复验，并形成基于实测的集中空间策略草案。**未启动统一开发工具实现，未更新任何制品 baseline。**

## 环境与支持范围（ST-M0.1）

| 能力 | 实测值/结论 | 证据 |
| --- | --- | --- |
| 宿主 | macOS 26.6.2 / 25G83，原生 arm64 | `environment.json` |
| Apple 工具 | CLT `/Library/Developer/CommandLineTools`，Apple clang 21.0.0 / clang-2100.1.1.101 | `environment.json`、`native-toolchain.json/log` |
| SDK | 26.5 / 25F70 | 同上，与仓库锁精确一致 |
| CMake | 4.4.2 | 同上 |
| Python | 3.13.15；编排设计下限仍为 3.11 | `environment.json`；本次未验证 3.11 |
| Node | 默认 22.12.0 不合格；本机已有 24.19.0，实验仅在进程 PATH 选择它 | `environment.json`、`doctor.json/log` |
| pnpm | 已有 9.15.9，与 `packageManager` 一致 | `doctor.log` |
| Rust | 1.97.1，已安装 `aarch64-apple-darwin` | `environment.json` |
| 依赖声明下限 | 当前 macOS 依赖图最高 `rust-version` 为 1.88（包括 time/image/plist/darling） | `cargo-metadata.json`、`rust-declared-baseline.json` |
| 实测 Rust 基线 | 1.97.1 的 `cargo check --workspace --locked --offline` 通过，83.42 秒 | `cargo-check.json/log` |
| Swift | Apple Swift 6.3.3，满足 Package.swift tools 6.0 | `environment.json`；冷构建另见部署实验 |

当前只能声明 **Rust 1.97.1 是本机已验证的支持基线**，不能把依赖声明的 1.88 当作本项目已验证最低版本；本机只安装一个 Rust toolchain，实验不安装第二套。Python 3.11、其他 Rust 版本和其他 CLT 宿主也没有本次实测证据。

完整 Xcode 26.5 的供应方宿主范围为 macOS Tahoe 26.2～26.x，SDK 为 26.5；本机使用 CLT，未验证完整 Xcode 路径。供应方兼容不等于本项目实测支持。CLT 安装下限不能由完整 Xcode 表推断。

官方来源（本轮读取）：[Apple Xcode 支持表](https://developer.apple.com/xcode/system-requirements/)、[CLT 安装说明](https://developer.apple.com/documentation/xcode/installing-the-command-line-tools/)。CLT 页面在网页文本工具中没有可用的宿主版本表，因此不据此新增安装下限承诺。

### 按任务划分的能力清单

- 帮助：系统 Bash 3.2 与基础命令；版本/状态/编排另需 Python 3.11+。
- 前端：合格 Node、pnpm、已提交 pnpm 锁；typecheck/Vitest/build 共享这些能力。
- Rust：已安装 Rust/Cargo、原生 arm64 目标、Cargo 锁、Apple 链接工具；proto 使用 protox，不需要另装 protoc。
- Swift helper：Swift tools ≥6.0、Package.resolved、已提交且协议摘要一致的 Swift 生成代码；实际构建必须强制 resolved versions。
- 原生 ASR：git/curl、Shell/Python、jq、固定 clang/C++/SDK/CMake、tar/stat/shasum、file/lipo/otool/vtool/install_name_tool，以及 source/artifact 锁。
- FFmpeg：固定 clang/SDK、make、归档与哈希工具、原生验证工具、锁定源码归档。
- 完整出包：以上构建能力，加 ditto、plist/资源/许可证校验以及交付目录的写入和空间条件。
- unit：前端和 Rust 测试能力，当前 checkout 的 FFmpeg；本工具 Python 测试仍为 M1 后新增项。
- integration/all：另需 API 项目的锁定 Node/Cargo 依赖、已有 Python 的 pip/jsonschema 项目包，以及真实 ASR；完整 Python 传递依赖锁仍待 M4 接入，不在实验中生成正式锁。

只读环境检查不得因合法缺失项目输入而阻塞。`cargo metadata --filter-platform aarch64-apple-darwin --locked --offline` 可以避免本轮无关 Linux 依赖展开；未过滤的平台解析曾尝试展开 alsa 并因沙箱缓存写入权限失败，该结果不是 macOS 构建失败证据。

## 原生部署与目录实验（ST-M0.2）

- 原有 FFmpeg：`vtool` 显示 `minos 26.0`；原产物保持原位。
- 从同一锁定 FFmpeg 9.0.1 归档冷构建，在现有脚本的隔离副本中加入编译、链接和 host 的 `-mmacosx-version-min=13.0`；环境同时设 `MACOSX_DEPLOYMENT_TARGET=13.0`。
- `ffmpeg-13-cold-v2` 完成，39.21 秒；`ffmpeg-13-macho` 为 `arm64/minos 13.0/SDK 26.5`；`ffmpeg-source-validation` 通过现有对应源码校验。
- 第一次尝试传相对 cache/output 路径，在脚本 `cd` 到源码树后复制归档失败。其临时输出已由原脚本清理，未触碰原 bundle。正式适配器必须传绝对路径，兼容入口的相对路径行为需要单独处理。

实验脚本将临时源码放到仓库内以采样资源，发现上游 `ffbuild/version.sh` 会向父目录寻找 Git 仓库，意外将本项目 Git 描述写入 FFmpeg 版本字符串。`ffmpeg-git-parent-probe.json` 对照：仓库内为 `OpenSeaSnail_0.1.0-1-g0199d5b`，仓库外为 `9.0.1`。这会让同一 FFmpeg 源码在不同位置或项目提交下产生不同字节；需要在正式构建策略中隔离父 Git 发现或显式固定上游支持的 revision 输入。`revision=9.0.1` 环境变量尝试被上游 Makefile 的局部赋值覆盖，两个 normalized 实验仍受父 Git 污染，不能当作修复证据。随后在两个全新目录设置 `GIT_CEILING_DIRECTORIES=<checkout root>`；isolated-a/b 均显示 9.0.1，二进制大小 3,765,160 字节、SHA-256 均为 `ec6d4045b32fd04ff0b25c6c1455e1009ce3d3173f7cef8eb116213ff9b71b38`。两次分别耗时 57.47/53.37 秒，架构、部署目标和系统 dylib 检查通过；未修改生产脚本或 baseline。`make_ffmpeg_recipe.py` 可重建本轮实验脚本，生成结果与实际执行副本逐字节比较通过。

Sherpa 输入在当前 checkout 完整校验通过（`verify-existing-inputs`，104.73 秒）。`sherpa-cold-new-path` 冷构建成功，638.68 秒；sidecar、ONNX Runtime 与 Sherpa 核心 dylib 的大小和 SHA-256 全部与 canonical manifest 一致，详见 `sherpa-native-comparison.json`。10 个实际冷构建 Mach-O 均为 arm64/minos 13.0（GUI、daemon、Swift helper、FFmpeg，以及 native install 中的探针/sidecar/dylib；不重复计 symlink），详见 `native-macho-inventory.json`。独立 CMake 缓存搬迁探针被 CMake 拒绝（exit 1），证明编译树不能作为可继续增量的原子发布对象。新路径实验复制这些经验证的当前 checkout 源码，再使用全新编译目录；没有复用旧 native install 或其他项目产物。该实验与单独从空目录联网获取的 `cold-inputs` 分开报告，复制输入不能冒充冷下载。

## 测试完整性与进程实验（ST-M0.4）

### 结果格式

- 前端 typecheck 通过，Vitest JSON 有 172 个真实用例，全部通过，可读取用例身份与状态。
- Rust `cargo test --workspace --no-run --locked --offline --message-format=json` 通过；编译 JSON 列出 30 个测试可执行文件。对每个文件仅运行 `--list --format terse` 与 `--ignored --list --format terse`，共枚举 760 个用例、10 个 ignored；没有执行 ignored 测试。
- 当前 stable harness 拒绝 `--format json`，退出 101，要求 nightly 与 `-Z unstable-options`。不能为了 JSON 自动安装或切换 nightly；M4 需使用枚举身份与受约束文本结果适配，解析失败即 incomplete。编译 JSON 不是执行结果。
- Python 标准 unittest 的自定义 TestResult 能收集身份、failure 与 skip；该格式探针有 3 个 **synthetic** 用例，不能当作项目业务或未来工具测试通过。
- API 现有结果适配器 7 个合成测试通过，覆盖失败、清理失败、flaky、skip/missing 等结果语义；不是 quick/acceptance 实际执行证据。
- 详细枚举和源码 marker 清单见 `framework-format-details.json`；另用 `cargo test --workspace --doc --locked --offline -- --list` 枚举七个 workspace crate，当前 doctest 数均为 0。枚举不能当作完整 workspace 执行通过。

### 已知隐式跳过与排除

daemon `sessions_pipeline` 中 15 个用例有 FFmpeg 不可用时的提前返回；runtime `normalizer_real` 的 3 个用例也有该路径。必须预检当前 checkout 的真实 FFmpeg，严格传入路径，禁止回退系统 FFmpeg。

额外阻塞：`normalize_mp3_input_to_16000` 使用 `libmp3lame` 编码生成测试输入，但随包 FFmpeg 明确只包含 PCM encoder。实际执行显示 `skip: mp3 编码失败` 后仍报告 `3 passed; 0 ignored`，其中 MP3 业务断言没有执行。另在 FFmpeg 不可用的隔离 PATH 下，3 个 normalizer 用例全部提前返回，仍显示 3 passed；而使用实验 FFmpeg 的 daemon sessions_pipeline 40 个用例通过。**仅注入 FFMPEG_PATH 不能满足默认测试无假绿要求。** 推荐在开发审查中改用有来源与哈希的固定 MP3 测试资产，保留原归一化断言；本轮不修改测试、外部库支持范围或生产二进制。

10 个 ignored 项：真实 Keychain 3 项；其他后端（diarize/whisper/FunASR）3 项；真实 Sherpa runtime 1 项；models_api 中真实 Whisper 1 项、真实 Sherpa 2 项。前两类及真实 Whisper 属于明确排除；默认 Sherpa 用例应说明其与 API acceptance 的覆盖关系，不能仅因 ignored 就一概算作已覆盖。

### 锁、硬中断与信号

`lock-process-tree-v3` 的 5 个受控实验均达到预期：

| 情形 | 实测结果 |
| --- | --- |
| controller 强杀、直接持锁的 middle 仍在 | 锁 busy，第二个修改者被阻止 |
| controller/middle 强杀、leaf 曾被 Python 默认关闭传入 fd | leaf 仍在，锁已释放，存在并发风险 |
| 各层显式 pass_fds，controller/middle 强杀 | leaf 仍在，锁 busy |
| SIGINT | 本组终止，controller 返回 130，随后可取锁 |
| SIGTERM | 本组终止，controller 返回 143，随后可取锁 |

实验只操作其创建并记录的 PID/进程组，无按名称查杀。每个 leaf 有 25 秒上限，finally 清理其自身进程组。

初版实验在信号 handler 内递归调用已经处于等待中的 Popen.wait，出现等待锁重入超时；第二版 marker 写入有读到半个 JSON 的竞态。原始失败日志保留。第三版信号 handler 只记录信号，由主循环完成停止/等待，marker 原子替换。生产实现需要同样避免信号 handler 中阻塞等待，但本实验不是生产信号管理器。

结论：设计中的“锁传给受控子进程”必须限定传递层级；不能假设上游 Python/CMake/工具会自动继续传递 fd。阶段监督进程可以保护顶层编排器死亡，但监督进程也被强杀时仍需残留进程证据和 fail-closed 门禁。这是 M1.4 的设计收紧项，当前未实施。

## 资源预算、完整性与复核

资源采样每 5 秒记录实验树已分配磁盘和整个卷剩余空间。并行实验下，整个卷变化不能归因于某一步；FFmpeg、Sherpa 子树单独采样。采样峰值可能漏掉短暂峰值，不能直接用样本最大值作为硬阈值。冷准备实验 `cold-inputs` 实际执行 820.37 秒后失败：163,002,883 字节的模型仅收到 76,511,628 字节便达到 curl 的 600 秒总超时。已验证的小归档保留，未完成模型临时文件由 fetch 的 finally 删除，最终模型未发布。该实测吞吐约 128 KB/s；固定 600 秒无法支撑当前网络上的完整模型获取。按用户建议，现有下载脚本的单文件总超时已从 600 秒调整为 3600 秒，连接超时仍为 30 秒，并新增连续 120 秒低于 1024 字节/秒的低速退出条件。TLS、锁定大小与 SHA-256 校验、验证后发布均保留。局部受控检查 `download-timeout-policy-check` 已通过：超时参数、已校验文件复用、curl 超时清理以及坏哈希拒绝。`cold-inputs-long-timeout` 在 376.39 秒后因 ONNX Runtime 的 Git clone 独立 300 秒超时失败；模型已完整下载并校验成功，没有用本地模型冒充成功下载。随后将 Git clone、稀疏检出、可能触发按需下载的 checkout 及子模块准备的单次上限同步调整为 3600 秒，本地 revision/status 查询仍为 300 秒。`download-and-source-timeout-check` 验证了这些边界；旧克隆路径的残留进程核验为 0。`cold-inputs-source-long-timeout` 复验通过（177.53 秒，退出码 0），复用已校验归档和 sherpa-onnx checkout，只重新获取未完成的 ONNX Runtime。完整 verify-locks 核验包含全部下载、模型归档内容、源码 revision、子模块及许可证。本次网络速度较上一轮更快，成功说明完整获取链路可行，并不证明曾持续超过 600 秒的模型下载现在也能通过。三轮从空输入目录开始的失败/恢复证据一并保留；这不是全新 clone、空标准包缓存的一次出包验收。

开始时保存了 630 个已跟踪业务/构建输入摘要（排除本需求目录）与 35 个原 FFmpeg/Sherpa 制品文件摘要；完成后逐个重新比较，在上一轮实验结束时全部不变（`input-protection-verification.json`）。随后按用户建议局部修改了现有 `scripts/sherpa/fetch-inputs.py` 的下载超时；原快照保留为历史证据。锁、质量规则、预算与 baseline 不允许因实验失败而更改。

重跑方式：先阅读每个 `<name>.json` 的实际 command/environment；实验采集器拒绝覆盖同名证据，应换新名称。长 native 实验使用全新输出路径，不 rename 或删除已有 CMake 编译树。不要直接把实验 FFmpeg 装入默认 bundle 或修改 API baseline 来凑通过。


### 本地部署、交付与资源结果

Sherpa staging 完整准备 110.00 秒后，仅 rename 最终 artifact；原编译树留在固定位置。发布路径的继承 FD sidecar smoke 通过（9.10 秒，包含实际锁定音频与两类空结果），native load-only 探针通过（3.14 秒）。Swift 强制 resolved versions 的独立 release 冷构建 377.07 秒，helper 自检和锁定 Rust 协议 smoke 均通过。

前端在隔离目录 frozen 安装 8.27 秒、构建 27.67 秒；允许使用已有标准下载缓存，不能称为空标准缓存首装。Rust workspace 的 release 冷编译 280.36 秒；前端最新输出完成后再次 locked/offline 构建 22.48 秒，GUI 二进制与之前及实验 App 内的摘要完全相同（`gui-frontend-recheck.json`）。

实验 App 使用本轮冷构建 GUI/daemon/helper、新路径 Sherpa、Git 隔离后的 FFmpeg。现有组装脚本的隔离副本仅调整 ROOT、已验证二进制/前端输出路径，使用 `--skip-rust-build`，未改变组装校验逻辑。组装 7.94 秒，收集 489 项依赖许可证。ZIP 创建 10.70 秒、解压 1.48 秒；431 项文件/目录/链接的摘要、目标和模式一致，随包 7 个 Mach-O 均 arm64/minos 13.0，App version 0.1.0。没有启动 GUI、签名、安装 App 或系统授权。

`app-zip-validation.json` 记录 App 文件总字节 345,443,165，ZIP 210,672,292 字节；ZIP SHA-256 为 `83c94a12245e52436787fd4e3341f748122280327c9db4b3b1658b69049ef3fe`。该目录只是 M0 组装与空间实验，不是 M3 的原子四项交付实现。

| 单独采样的范围 | 阶段 | 样本最大已分配空间 |
| --- | --- | --- |
| FFmpeg isolated-a 工作树 | 解包、配置、编译、bundle | 155.93 MiB |
| Sherpa native-work（含复制源码） | 冷编译及 install | 1468.39 MiB |
| Sherpa native-work（同树累积） | staging 准备 | 1750.79 MiB |
| 前端隔离树 | frozen 安装/构建 | 331.51 MiB |
| delivery | App 组装 | 317.57 MiB |
| delivery（累积） | ZIP 创建 | 539.38 MiB |
| delivery 最终树 | App + ZIP + 解压副本 | 0.842 GiB |

采样与最终持久占用分开：解压阶段短于 5 秒，其阶段样本只有解压前值，最终占用补充了两个 App 副本的实际空间。整个实验树最后为 11.902 GiB，其中 Rust target 7.947 GiB（同时保留 debug、测试、release）、native-work 1.698 GiB、Swift build/cache 0.922 GiB。重复 FFmpeg 试验、源码副本、测试输出和原始证据都计入总树；没有据此猜测正常 package 的最低空间。资源明细见 `resource-observations.json`。

API 的真实 Sherpa 预检使用实验 App 的新 ASR、实验 FFmpeg 与原固定 baseline，确实返回 `sherpa_ffmpeg_hash_mismatch`。这证明新 FFmpeg 不能直接接入当前 acceptance；没有改 baseline、质量规则或预算，没有将预检的预期拒绝算成业务验收通过。完整 quick/acceptance 留给受审 baseline 接入阶段。

### 实施前需收紧的事项

1. 本轮按用户建议局部调整现有下载及源码准备超时，并增加 curl 低速退出条件；集中空间策略草案见下文，生产检查尚未实现。
2. 用有来源和哈希的固定 MP3 测试资产保留原断言，避免依赖生产包不包含的编码器；继续显式处理其他 ignored 的登记与覆盖关系。
3. FFmpeg 原生身份包含阶段内的 Git 发现隔离，许可证材料包含真实完整构建策略；不得全局修改开发者 Git 环境。
4. 阶段监督者持锁，持久记录受控进程组和身份；锁已释放但旧运行未终结时仍核验残留，无法证明无活跃执行者就阻塞。不能仅依赖顶层 PID 或默认 fd 继承。

除上述现有下载脚本的局部超时调整外，其他设计收紧建议尚未实现；未启动统一工具实现。M0 关键实验已补齐，但未把目标系统实机、空标准缓存出包或完整业务验收标为通过。

### 冷准备复验与集中空间策略草案

新证据 `resource-observations-after-timeout.json` 不覆盖原快照：冷准备采样最大 944.43 MiB，最终输入树 0.907 GiB（下载缓存 0.218 GiB、源码 0.689 GiB）；整个实验树最终 12.743 GiB。冷准备与先前固定路径的冷编译分开执行，两者均核验同一锁定输入；未重复整套编译，也不宣称这是一条统一 package 命令的成功。

[resource-policy.json](resource-policy.json) 集中记录各阶段证据、余量和限制。每个阶段按“可归因的样本最大值与最终树占用取较大值 × 1.5，向上取整到 GiB，至少 1 GiB”分配预约空间；Swift 合并 build/cache 的最终占用，Cargo 采用 426 个 macOS 锁定依赖源码目录的只读测量（319,877,120 字节）。这些是明确的策略余量，不是实测峰值或最低空间证明。

| 预约范围 | 草案预算 |
| --- | --- |
| 锁定归档与源码 | 2 GiB |
| Cargo 依赖源码缓存 | 1 GiB |
| FFmpeg 工作树 | 1 GiB |
| Sherpa 解包、编译、staging | 3 GiB |
| 前端依赖与构建 | 1 GiB |
| Swift build/cache | 2 GiB |
| Rust debug、测试、release | 12 GiB |
| App、ZIP 与解压验证 | 2 GiB |
| 同卷冷运行合计 | **24 GiB** |

该草案适用于本次工具链、单运行、串行阶段、已有合格 Cargo/pnpm 标准缓存。准备前按实际目标卷累加预算；每个长阶段前复查该阶段及剩余阶段预约量，缓存位于其他卷时分别检查；交付前完整复查 2 GiB 的交付预约。未证明有效的缓存不能扣减预算。旧交付若超出预约需另计；不删除旧包来满足空间检查。Rust 预算刻意覆盖 debug/测试/release，原生源码存在保守重复计入；不能把 24 GiB 解释为 package 最低要求。

5 秒采样可能漏峰值，工具链/源码变更或并行阶段必须重新测量；空全局 Cargo/pnpm 缓存的下载/解包预算仍由 M7 验证并修订。ST-M0.3 的实测与集中策略草案已交付，生产磁盘门禁属于后续实现。
