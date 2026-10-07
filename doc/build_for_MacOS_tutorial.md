# SeaSnail macOS 本地构建与出包教程

本文面向首次构建 SeaSnail 的开发者，目标是在 macOS 13+ Apple Silicon（arm64）上准备 Sherpa ONNX 运行时，并生成一个未签名的开发版 `.app`。正式分发还需要单独完成 Developer ID 签名、公证和 staple；本教程不覆盖发行签名。

所有命令从仓库根目录执行。两条构建路径：已有与当前 catalog 匹配的 Sherpa artifact 时，直接按第 4 节复用；没有制品时，先按第 2、3 节准备锁定输入并构建。`--package` 不会自动下载 Sherpa 的全部源码、子模块、依赖归档和模型。

[English development guide](development.md) · [中文开发指南](development.zh-CN.md)

## 1. 前置条件

需要安装并确认以下工具。可先运行 `python3 scripts/doctor.py` 获得缺失项与 Node 版本诊断：

原生运行时还要求 [Sherpa 构建输入说明](../scripts/sherpa/BUILD-INPUTS.md) 中固定的 Apple clang、macOS SDK 和 CMake 版本；环境检查会提前拒绝不匹配的工具链。macOS 13+ 是 App 的运行目标，开发机器还必须能安装这套构建工具。

```bash
sw_vers
uname -m                 # 应为 arm64
xcode-select -p
cargo --version
node --version           # 22.x >=22.22.2、24.x >=24.15 或 26+（按当前锁文件）
pnpm --version           # 9+
jq --version
python3 --version
cmake --version          # 从源码构建 Sherpa 时需要
```

如果 Rust 是通过 rustup 安装的但当前终端找不到 `cargo`：

```bash
source "$HOME/.cargo/env"
```

获取代码并安装前端依赖：

```bash
git clone https://github.com/0Litost0/SeaSnail.git SeaSnail
cd SeaSnail
pnpm --dir apps/desktop install --frozen-lockfile
pnpm --dir apps/desktop generate:openapi
```

## 2. 准备并校验 Sherpa ONNX 输入

默认出包使用 Sherpa ONNX SenseVoice int8 native sidecar。获取源码、子模块和缓存文件需要事先完成；后续校验与 Sherpa 构建按离线输入执行。仓库不提交大型源码、模型和二进制；构建输入由 `scripts/sherpa/source-lock.json`、`artifact-lock.json` 及 [BUILD-INPUTS.md](../scripts/sherpa/BUILD-INPUTS.md) 约束。

首次获取可以使用锁定下载脚本（需要联网，不覆盖不匹配的现有输入）：

```sh
python3 scripts/sherpa/fetch-inputs.py --list
python3 scripts/sherpa/fetch-inputs.py
```

脚本准备下列默认目录并自动执行离线校验。也可以手动准备以下本地输入：

- 按 lock 文件指定版本 checkout 的干净 Sherpa ONNX 源码；
- 按 lock 文件指定版本 checkout 的干净 ONNX Runtime 源码及其子模块；
- lock 文件列出的全部归档和模型文件，放入同一个 cache 目录。

统一入口默认查找以下目录；若使用其他位置，可通过 `--sherpa-cache`、`--sherpa-source` 和 `--onnxruntime-source` 指定：

```text
third_party/sherpa/macos-arm64/
├── cache/                  锁文件中的依赖归档与模型
└── sources/
    ├── sherpa-onnx/        锁定的干净源码 checkout
    └── onnxruntime/        锁定的干净源码 checkout，含子模块
```

然后执行离线校验（示例路径需替换为实际输入路径）：

```bash
scripts/sherpa/verify-locks.sh \
  --cache /path/to/verified/cache \
  --source /path/to/clean/sherpa-onnx \
  --onnxruntime-source /path/to/clean/onnxruntime
```

校验器会检查版本、源码状态、子模块、SHA-256、文件大小和模型条目。校验失败时不要跳过检查，也不要用系统已安装的 ONNX Runtime 或未锁定的模型目录替代。

如果已有与当前 `crates/daemon/resources/models.json` 中 manifest 哈希和大小匹配的 Sherpa artifact，可以跳过源码、cache 和 native install 准备，直接执行第 4 节；`--app` 还要求已有 FFmpeg bundle，`--all` 会准备 FFmpeg。例如：

```bash
scripts/build.sh --app \
  --asr-root /absolute/path/to/verified-sherpa-artifact \
  --output dist/SeaSnail-dev.app
```

该快捷路径仍会重新构建前端和 Rust，并在组装 App 时重新校验 artifact；它不会自动下载或重建 Sherpa 源码。

## 3. 构建 Sherpa artifact

先用 probe 构建 native install，再组装应用需要的 artifact：

```bash
scripts/sherpa/build-probe-macos-arm64.sh \
  --source /path/to/clean/sherpa-onnx \
  --onnxruntime-source /path/to/clean/onnxruntime \
  --cache /path/to/verified/cache \
  --output third_party/sherpa/macos-arm64/build/probe

scripts/sherpa/prepare-macos-arm64.sh \
  --source /path/to/clean/sherpa-onnx \
  --onnxruntime-source /path/to/clean/onnxruntime \
  --cache /path/to/verified/cache \
  --native-install third_party/sherpa/macos-arm64/build/probe/install \
  --output third_party/sherpa/macos-arm64/artifact
```

`prepare-macos-arm64.sh` 会根据仓库中的固定 artifact manifest 校验 sidecar、模型、VAD、native library 和许可证材料。仅有相同源码版本不保证生成相同二进制；构建结果仍必须通过哈希校验。输出目录已经存在时不会覆盖，请改用新的输出目录，并将新 artifact 路径传给后续 `--asr-root`。

## 4. 生成未签名开发 App

### 推荐：一条命令出包

已有匹配 artifact，或已按第 2 节在默认目录准备好锁定 cache 和源码 checkout 时，可以执行以下命令；如果已手动完成第 3 节，会直接复用生成的 artifact：

```bash
scripts/build.sh --package --output dist/SeaSnail-dev.app
```

该命令会依次复用或构建 Sherpa artifact、复用或构建 ffmpeg、构建前端和 Rust，最后组装未签名 App。所有 Sherpa 中间产物默认保存在 `third_party/sherpa/macos-arm64/` 下。已有 artifact 不匹配时不会自动替换；应指定已验证的新制品。仅执行 `--prepare-runtime` 且提示“复用”也不代表已完成 App 组装阶段的 catalog 和文件校验。

### 分步出包

日常开发出包使用 `--app`：

```bash
scripts/build.sh --app \
  --asr-root third_party/sherpa/macos-arm64/artifact \
  --output dist/SeaSnail-dev.app
```

`--app` 会构建前端、编译 GUI/daemon 的 release 二进制并组装 App。它要求已经存在可用的 ffmpeg bundle；如果还没有准备 ffmpeg，请先执行 `scripts/build.sh --ffmpeg`，或者直接使用下一节的 `--all`。`--output` 必须指向不存在的路径。

### 完整出包

需要构建整个 Rust workspace，或需要同时准备内置 FFmpeg 时，使用 `--all`。`--app` 本身也会重新编译 GUI/daemon，因此普通 Rust 业务修改无需仅因代码变化就切换到 `--all`：

```bash
scripts/build.sh --all \
  --asr-root third_party/sherpa/macos-arm64/artifact \
  --output dist/SeaSnail-all.app
```

`--all` 等价于前端 + Rust workspace + ffmpeg + App 组装。ffmpeg 首次构建会从锁定的源码地址下载并编译静态 arm64 可执行文件，之后复用本地缓存；运行时不依赖 Homebrew。

也可以分步执行：

```bash
scripts/build.sh --frontend
scripts/build.sh --rust
scripts/build.sh --ffmpeg
scripts/build.sh --app --asr-root third_party/sherpa/macos-arm64/artifact \
  --output dist/SeaSnail-dev.app
```

查看全部选项：

```bash
scripts/build.sh --help
```

## 5. 启动和验证

```bash
open dist/SeaSnail-dev.app
dist/SeaSnail-dev.app/Contents/MacOS/SeaSnail --tcc-status
```

首次录音需要麦克风权限，自动向焦点应用注入文字需要辅助功能权限。未签名开发包被 Gatekeeper 拦截时，仅在确认构建来源可信的情况下手动放行。

完成开发后按[根 README 的测试与 Agent 开发约定](../README.md#测试与-agent-开发约定)选择检查，API 相关修改还需执行对应回归；App 构建成功不能替代业务测试。

未签名包包含 `SEASNAIL_DEV_FILE_KEYCHAIN` 标记，会启用普通文件 Keychain fallback，仅供开发和内部测试。正式发行必须使用独立的签名、公证流程，并确保移除开发标记。

日志默认位于：

```text
~/Library/Application Support/SeaSnail/logs/
```

隔离测试可以在启动 daemon 前设置 `SEASNAIL_DATA_DIR`，将日志、锁和 sidecar 孤儿记录放到指定数据根。

## 6. 常见问题

| 现象 | 处理方式 |
| --- | --- |
| `cargo: command not found` | 执行 `source "$HOME/.cargo/env"`，或安装 Rust stable。 |
| Sherpa 输入校验失败 | 检查源码是否为指定 revision、checkout 是否干净、cache 文件的 SHA-256 和大小是否正确。 |
| `Sherpa artifact manifest does not match the embedded catalog` | 更换与当前 catalog 匹配的已验证 artifact；已有旧制品不会被自动替换，不要跳过哈希校验。 |
| `Sherpa artifact root not found` | 先执行第 3 节，或通过 `--asr-root` / `SEASNAIL_SHERPA_ARTIFACT_ROOT` 指定 artifact。 |
| `--output` 已存在 | 改用新的 `.app` 输出路径；脚本不会覆盖既有产物。 |
| `--all` 首次构建 ffmpeg 较慢 | 属于首次下载和本地编译，等待完成；后续构建会复用缓存。 |
| App 无法连接本地服务 | 查看 `~/Library/Application Support/SeaSnail/logs/backend.*`，确认没有残留的 `bootstrap.json`。 |
| 无法录音或自动粘贴 | 在系统设置中检查麦克风和辅助功能权限，并重新运行 `--tcc-status`。 |

## 7. 兼容候选

FunASR 和 GGUF 仍保留在统一构建入口中，用于兼容性验证，不是当前默认出包路径。需要构建兼容候选时，查看：

```bash
scripts/build.sh --help
```

并显式传入 `--funasr-root` 或 `--gguf`；不要把兼容候选的 Python、模型或 bundle 步骤混入 Sherpa 默认出包流程。

## 参考

- [README](../README.md)
- [Sherpa 构建输入说明](../scripts/sherpa/BUILD-INPUTS.md)
- [第三方许可证清单](../scripts/sherpa/licenses/THIRD-PARTY-NOTICES.md)
- [macOS App 构建脚本](../scripts/macos/build-dev-app.sh)
- [统一构建入口](../scripts/build.sh)
