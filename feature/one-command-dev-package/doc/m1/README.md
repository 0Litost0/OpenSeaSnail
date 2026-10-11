# M1 实施与复核

2026-10-11：公共入口、只读检查、schema-v1 运行记录、仓库锁/阶段监督及 runs/status 已接入。build/test/package/version 实际能力在后续里程碑接入，当前明确返回 operation_unavailable/6，不能空执行成功。

验证入口：`PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s scripts/dev_package/tests -v`。测试使用临时目录；原始当前 checkout 的检查结果和本轮实现验证输出位于忽略的 `dist/dev-package/m1-20261011/`。

- Bash 3.2 根帮助不要求 Python/Node；引导缺 Python 的 JSON 字符串转义有隔离检查。
- 参数组合、UTC since、limit、run_id 和纯三段版本语法在写入前校验。
- 支持的 Node 分支从 frontend package.json 读取；原生身份从 native-toolchain.json 读取；Rust/宿主及资源策略在 policy.json 集中声明，基于 M0 实测，不承诺空全局缓存或旧宿主已验证。
- 原 doctor 路径复用 checks，保留 frontend-only 行为。check 不创建目录、不下载；缺少合法输入属于 missing_preparable，损坏或未知输入阻塞。
- 原子 state/result、持久 events 和提前 run_started 通知；JSON 模式 stdout 只有最终对象。修改任务独立运行目录，查询不修复状态或创建记录。
- 每阶段监督进程继承同一个 flock，在启动工具前落盘进程组。监督进程被强杀、工具未继承 fd 的情况仍由持久进程组门禁阻塞；不能证明残留已消失就不放行，不按名称查杀。
- 当前 M1 尚不执行业务构建，环境检查结果不等于出包成功。明确记录未集成命令的失败。

M1 完成，进入 M2。子代理 `review_m1` 首轮不放行，已修复并复核：

1. Corepack/rustup 禁网络仍初始化空缓存；改为静态解析已安装工具，直接调用现成二进制。真实空缓存复验不创建任何路径，缺工具明确阻塞。检查环境继续禁止安装/自动 pin，Git 只读查询禁止 lazy fetch。Apple 工具选择不存在时不调用会提示安装的 compiler/Swift/xcrun。
2. 阶段 poll 返回已退出瞬间的 SIGTERM 现在仍返回 Interrupted，而非 0。
3. 损坏 Sherpa 声明返回 input_invalid/5。
4. FFmpeg 父路径 symlink 在访问前拒绝；Sherpa 固定文件 hash 单列验证范围，主制品缺托管凭据/许可证来源核验时为 missing_preparable，未知额外内容阻塞。
5. 管理器 settings/package/bin metadata 类型显式校验，避免非法结构导致无 JSON 的 traceback。

最终 34 项测试通过（tests-final.log），git diff --check 通过。结构和进程测试不替代后续真实原生构建；尚未接入的任务继续如实失败。未修改现有模型/Sherpa/质量/版本 baseline。

安装行为依据：[rustup 环境变量](https://rust-lang.github.io/rustup/environment-variables.html)、[Corepack 官方说明](https://github.com/nodejs/corepack/blob/main/README.md)；空缓存写入副作用以本机隔离复验为准，未通过设置临时 HOME 掩盖副作用。
