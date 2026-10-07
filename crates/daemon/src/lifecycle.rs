//! 守护进程生命周期：stdin 管道 EOF 父进程死亡检测。
//!
//! GUI 壳 spawn 守护进程时将其 stdin 设为管道读端（GUI 持写端）。
//! GUI 退出/崩溃 → OS 关闭写端 → 守护进程 stdin 读到 EOF → 触发优雅关闭。
//! 这等价于设计文档「让守护进程继承一管道读端，阻塞 read，EOF 即父退」，
//! 用 stdin 本身作该读端，免去自定义 fd 传递（os_pipe API 为 ST-M1.1 上机项，
//! stdin 方案同样满足「管道 EOF 即父退」语义且更稳）。

use std::io::Read;
use std::sync::Arc;
use tokio::sync::Notify;

/// 启动父进程死亡监听：在独立标准线程阻塞读 stdin（不阻碍 Tokio runtime 退出），EOF 即父进程消失，
/// 调 `shutdown.notify_one()` 触发优雅关闭。守护进程日常 stdin 不收数据，仅以 EOF 为信号。
pub fn spawn_parent_death_watcher(shutdown: Arc<Notify>) {
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut handle = stdin.lock();
        let mut buf = [0u8; 1];
        loop {
            match handle.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {} // 守护进程 stdin 不应有数据；偶发字节忽略，仅 EOF 有意义
            }
        }
        tracing::info!("stdin EOF：父进程消失，触发优雅关闭");
        shutdown.notify_one();
    });
}
