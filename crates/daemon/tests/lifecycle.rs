//! 集成验收（ST-M1.1–M1.6）：
//! M1.1 ① spawn → 健康检查通过 ② 父进程死亡（stdin EOF）→ 守护进程自退 ③ SIGTERM 优雅关闭
//! M1.2 ④ 守护进程启动写 bootstrap；退出清 bootstrap
//! M1.4 ⑤ `GET /api/v1/auth/status`（免鉴权）→ 200 `{initialized:false}`
//! M1.5 ⑥ 前端 X-Trace-Id 贯穿后端日志
//! M1.6 ⑦ 未匹配 OpenAPI 路由 → 404 `{error:{code,message}}`（统一错误 shape）

use seasnail_daemon::{
    bootstrap::Bootstrap, exit_code, http_get, http_get_with_headers, spawn_daemon, wait_ready,
};
use std::time::Duration;
use tokio::process::Child;

/// Cargo provides the exact binary path at compile time.
fn bin_path() -> String {
    env!("CARGO_BIN_EXE_seasnail-daemon").to_owned()
}

/// spawn 守护进程到独立临时 home。
async fn spawn(home: &std::path::Path) -> Child {
    spawn_daemon(&bin_path(), home)
        .await
        .expect("spawn 守护进程")
}

/// 验收①② + M1.2：spawn → bootstrap 就绪健康；关闭 stdin 写端 → 守护进程自退；退出后 bootstrap 已清。
#[tokio::test]
async fn spawns_healthy_and_exits_on_parent_death() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = spawn(dir.path()).await;
    let port = wait_ready(dir.path(), Duration::from_secs(5))
        .await
        .expect("bootstrap 应在超时内就绪");

    // bootstrap 已写入，端口与读回一致。
    let info = Bootstrap::new(dir.path().to_path_buf())
        .read()
        .expect("bootstrap 应存在");
    assert_eq!(info.port, port);

    // 模拟 GUI 崩溃/退出：关闭 stdin 写端 → daemon 读到 EOF → 自退。
    drop(child.stdin.take());
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("守护进程应在 stdin EOF 后 5s 内自退")
        .expect("wait");
    assert!(status.success(), "守护进程应 exit 0 退出");

    // 退出后 bootstrap 已清理。
    assert!(
        Bootstrap::new(dir.path().to_path_buf()).read().is_none(),
        "退出后 bootstrap 应被清理"
    );
}

/// 验收③（补充）+ M1.2：SIGTERM 触发优雅关闭，退出后 bootstrap 已清。
#[cfg(unix)]
#[tokio::test]
async fn exits_on_sigterm() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = spawn(dir.path()).await;
    let _port = wait_ready(dir.path(), Duration::from_secs(5))
        .await
        .expect("bootstrap 应就绪");

    let pid = child.id().expect("pid");
    std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("kill -TERM");

    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("SIGTERM 后应在 5s 内退出")
        .expect("wait");
    assert!(status.success(), "SIGTERM 应触发优雅关闭 exit 0");
    assert!(
        Bootstrap::new(dir.path().to_path_buf()).read().is_none(),
        "SIGTERM 退出后 bootstrap 应被清理"
    );
}

/// 验收 ST-M1.3：第二实例 flock 拒绝并以明确退出码退出。
#[tokio::test]
async fn second_instance_rejected_with_already_running_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut first = spawn(dir.path()).await;
    let _port = wait_ready(dir.path(), Duration::from_secs(5))
        .await
        .expect("首实例应就绪");

    // 同 data_dir 起第二实例 → 应在锁上拒绝并退出码 ALREADY_RUNNING。
    let mut second = spawn(dir.path()).await;
    let status = tokio::time::timeout(Duration::from_secs(5), second.wait())
        .await
        .expect("第二实例应在 5s 内退出")
        .expect("wait");
    assert!(!status.success(), "第二实例不应成功");
    assert_eq!(
        status.code(),
        Some(exit_code::ALREADY_RUNNING),
        "第二实例应退出码 ALREADY_RUNNING(2)"
    );

    // 清理首实例。
    drop(first.stdin.take());
    let _ = first.wait().await;
}

/// 验收 ST-M1.4：`GET /api/v1/auth/status` 免鉴权返回 200 + `{initialized:false}`。
#[tokio::test]
async fn auth_status_endpoint_returns_uninitialized() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = spawn(dir.path()).await;
    let port = wait_ready(dir.path(), Duration::from_secs(5))
        .await
        .expect("bootstrap 应就绪");

    let (code, body) = http_get(port, "/api/v1/auth/status")
        .await
        .expect("http get");
    assert_eq!(code, 200, "/auth/status 应返回 200");
    let v: serde_json::Value = serde_json::from_str(body.trim()).expect("响应体应为 JSON");
    assert_eq!(
        v,
        serde_json::json!({"initialized": false}),
        "M1 无账户存储，应返回 {{initialized:false}}"
    );

    // 清理。
    drop(child.stdin.take());
    let _ = child.wait().await;
}

/// 读取日志目录下所有 `backend.log*` 内容（拼接）。供日志验收轮询用。
fn read_all_logs(dir: &std::path::Path) -> String {
    let mut out = String::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if let Ok(content) = std::fs::read_to_string(e.path()) {
                out.push_str(&content);
            }
        }
    }
    out
}

/// 验收 ST-M1.5（trace_id 关联）：带 `X-Trace-Id` 的请求，其 trace_id 出现在 backend.log，
/// 证明前端请求 ↔ 后端日志经 trace_id 串联。
#[tokio::test]
async fn trace_id_appears_in_backend_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = spawn(dir.path()).await;
    let port = wait_ready(dir.path(), Duration::from_secs(5))
        .await
        .expect("bootstrap 应就绪");

    let trace_id = "trace-test-abc-123";
    let (code, _body) =
        http_get_with_headers(port, "/api/v1/auth/status", &[("X-Trace-Id", trace_id)])
            .await
            .expect("http get");
    assert_eq!(code, 200, "带 X-Trace-Id 的请求仍应 200");

    // non_blocking 后台 flush 有延迟 → 轮询 backend.log* 至出现 trace_id。
    let logs_dir = dir.path().join("logs");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut found = false;
    while tokio::time::Instant::now() < deadline {
        if read_all_logs(&logs_dir).contains(trace_id) {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(found, "backend.log 应含前端所带 trace_id={trace_id}");

    // 清理。
    drop(child.stdin.take());
    let _ = child.wait().await;
}

/// 验收 ST-M1.6：未匹配 OpenAPI 路由经 fallback 返回统一错误 shape `{error:{code,message}}`。
#[tokio::test]
async fn unmatched_api_route_returns_unified_error_shape() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = spawn(dir.path()).await;
    let port = wait_ready(dir.path(), Duration::from_secs(5))
        .await
        .expect("bootstrap 应就绪");

    let (code, body) = http_get(port, "/api/v1/no-such-route")
        .await
        .expect("http get");
    assert_eq!(code, 404, "未匹配路由应返回 404");
    let v: serde_json::Value = serde_json::from_str(body.trim()).expect("响应体应为 JSON");
    assert_eq!(v["error"]["code"], "not_found", "code 应为 not_found");
    assert!(
        v["error"]["message"].is_string() && !v["error"]["message"].as_str().unwrap().is_empty(),
        "message 应为非空字符串"
    );

    // 清理。
    drop(child.stdin.take());
    let _ = child.wait().await;
}
