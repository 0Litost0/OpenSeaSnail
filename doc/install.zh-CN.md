# 安装与体验 SeaSnail

[English](install.md)

当前原生打包流程支持 macOS 13+、Apple Silicon，不支持 Intel Mac、Windows 或 Linux。

先查看具体 [Release](https://github.com/0Litost0/SeaSnail/releases) 的可用资产、校验值、签名状态和已知问题。没有公开构建时，请按[源码开发指南](development.zh-CN.md)操作。

1. 从可信的 Release 下载 macOS arm64 包，通过 `shasum -a 256 <下载文件>` 与该版本提供的校验值比对。
2. 解压，把 SeaSnail.app 移入 Applications 并打开。若 macOS 阻止未签名包，仅在核实来源后使用系统提供的显式放行流程；以当前系统与该版本说明为准。校验值本身不证明发布者身份。
3. 填写本机用户名和密码，点击“查看隐私与权限说明”。此时尚未创建账户，也不会访问钥匙串，请保管好密码。
4. 阅读钥匙串用途后，点击“创建账户并启用安全存储”。系统可能请求访问 `com.seasnail` 钥匙串条目。麦克风与辅助功能各有独立授权按钮，可以暂时跳过，稍后在设置中授权。
5. 查看快捷键与剪贴板上下文指南，打开文本编辑器放置光标，按 **⌘ ⇧ Space**，说话，再按一次结束。无法自动粘贴时可手动粘贴或查看会话历史。

当前仓库脚本生成带文件 Keychain fallback 的未签名开发包，用于本地和内部测试。请阅读[隐私说明](privacy.zh-CN.md)与[发布检查表](releasing.md)，不要将其表述为已签名正式发行包。

## 常见问题

- 无法录音：检查「系统设置 → 隐私与安全性 → 麦克风」。
- 无法自动粘贴：检查同一位置的「辅助功能」，可先手动粘贴或查看历史。
- 快捷键冲突：在「设置 → 通用」修改录音快捷键。
- 转写失败：保留会话用于重试，检查脱敏日志，不要公开上传正文。
- 默认日志：`~/Library/Application Support/SeaSnail/logs/`，分享前移除个人路径和数据。

## 卸载

先退出 SeaSnail，再删除应用。删除应用不会自动删除账户数据。若要永久移除本地历史，先导出所需内容，再自行删除 `~/Library/Application Support/SeaSnail/` 或自定义的 `SEASNAIL_DATA_DIR`，此操作不可恢复。Keychain 项目另行管理，不同签名身份和开发 fallback 位置可能不同，请勿删除无关凭据。
