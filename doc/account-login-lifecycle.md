# 桌面账户登录与退出

Account & data 展示当前账户和“退出登录”。账户创建、登录已有账户统一放在退出后的入口；不再在设置中提供 Additional account 或直接切换账户的表单。已有其他本机账户的删除入口保留，仍需确认，且不能删除当前账户。

退出成功后：

- 清除当前账户的内存解密材料、原生代理凭据以及 Keychain 中的 Master DEK / root bearer；清除 registry 的自动恢复账户标记。
- 保留账户登记、加密数据库、会话、词典、资源和 provider 配置。重启应用仍停留在登录入口。
- 清空前端账户业务查询并取消未完成查询，避免上一账户数据出现在新账户页面。
- 登录已有账户需要密码；新建账户需要用户名、密码和密码确认。登录后重新建立账户数据上下文。首次安装仍使用原来的账户创建和系统权限引导。

录音启动、录音、提交、转写、清理和自动粘贴期间禁止退出；原生层使用录音启动锁和任务阶段判断，daemon 另以账户 lease 拒绝仍有业务操作的退出请求。失败不会进入登录页。

## 已确认的权限调整

本次用户批准允许**受信任桌面入口**在退出状态查询账户名称及创建新账户。公开 OpenAPI 保持原有权限：`GET/POST /api/v1/accounts` 仍需 root 权限，公开 unlock 仍按密码认证。退出不会删除数据库中的集成令牌；退出期间受保护操作因账户锁定被拒绝，重新登录沿用原有令牌策略。

桌面入口使用 `/internal/desktop-auth/{status,login,create,logout}`，不属于公开集成 API。daemon 启动时在数据目录生成随机 256-bit capability，以原子替换的 0600 文件保存；原生层读取时检查所有者、权限、长度并拒绝符号链接。每次 daemon 启动轮换，HTTP 校验 capability；普通 bearer 不能替代它。WebView 仅能通过主窗口受限 Tauri 命令调用，响应只包含账户名称、ID 和登录状态，不返回 capability、DEK 或 root bearer。能力密钥的隔离边界是本机 OS 用户，与现有本机凭据权限模型一致。

## 验证位置

- `crates/daemon/tests/desktop_auth.rs`：内部入口鉴权、公开 API 权限不变、退出后重启不自动登录、错误密码、重新登录和新建账户，以及 capability 文件轮换与权限。
- `crates/daemon/src/application/mod.rs`：存在账户 lease 时拒绝退出。
- `apps/desktop/src/features/accounts/LoginScreen.test.tsx`：登录、新建账户密码确认、失败清空密码、设置退出及账户缓存刷新。

开发包仍需在 macOS 上验证真实 Keychain、快捷键和登录页面交互；内存 Keychain 测试不替代签名发行包验收。
