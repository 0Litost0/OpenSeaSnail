//! OpenAPI 服务面：SeaSnail 唯一集成面，所有端点挂载于 `/api/v1` 前缀下。
//!
//! 对应 `proto/openapi.yaml`（`servers.url = …/api/v1`）。资源按子模块分组
//!（auth / accounts / tokens / sessions / models / export …），各里程碑逐步填充。
//! M1 仅 `auth::routes()` 提供 `GET /auth/status`；M2 已统一接入唯一 `HttpState`
//! 组合根，并落地鉴权端点（setup/password/accounts/unlock/tokens）。
//!
//! 本模块返回的子路由**不含** `/api/v1` 前缀；前缀由 `server::build_app` 统一 nest，
//! 使资源子模块只关心自身相对路径（与 openapi.yaml 的 `paths` 一致）。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use axum::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::Router;
use seasnail_runtime::{AudioNormalizer, RuntimeOperationGate, SidecarRegistry};
use tokio::sync::Semaphore;

use crate::account::{Auth, Storage};
use crate::application::{CallerContext, ModelCatalog};
use crate::cleanup::CleanupService;
use crate::error::AppError;
use crate::model_settings::ModelSettings;

pub mod accounts;
pub mod auth;
pub(crate) mod desktop_auth;
pub mod dictionary;
pub mod dto;
pub mod export;
pub mod models;
pub mod reasoning;
pub mod sessions;
pub mod tokens;

/// Supported composition boundary. The production launcher never registers a fixture factory.
#[derive(Clone)]
pub struct RuntimeFactory {
    pub installed: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub build:
        Arc<dyn Fn(&str) -> std::io::Result<Arc<dyn seasnail_runtime::ModelRuntime>> + Send + Sync>,
}

/// 应用共享态。`Arc<Auth>`（鉴权编排）+ ST-M3.5 起接入运行时句柄：`Storage`
/// 数据面、`SidecarRegistry`（活跃 ASR 后端）、运行时 gate（single-flight）、
/// `AudioNormalizer`（ffmpeg 归一）。
///
/// 进程组合输入。HTTP handler 不直接接触其中的底层资源；`HttpState` 只保留应用服务
/// 和必要的 transport-local 限流状态。
#[derive(Clone)]
pub struct AppState {
    auth: Arc<Auth>,
    storage: Arc<Storage>,
    registry: Arc<SidecarRegistry>,
    /// M4.1 唯一 primary ASR 物理门禁；scheduler 与 runtime facade 共享此实例。
    gate: Arc<RuntimeOperationGate>,
    normalizer: Arc<AudioNormalizer>,
    runtime_factory: Option<RuntimeFactory>,
    /// ST-M1.2 全局 component-keyed 模型设置（punc/spk enabled 开关，落 `<DataDir>`）。
    settings: Arc<ModelSettings>,
    /// 同一会话的编辑/删除串行化。弱引用使已完成会话不会永久占用 map 条目。
    session_mutation_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    /// ST-M4.4：注入计划一次性消费集（进程内）。首读插入返回 true、再读 410 Gone。
    /// 仅 root 可达（端点 root-only，且不在 `validate_api_request` 白名单，WebView 不可达）。
    injection_plan_consumed: Arc<Mutex<HashSet<String>>>,
    /// ST-M4 按需下载进度（进程内）：key=组件 id（punc），val=0.0–1.0。
    /// 下载完成/失败后清条目；downloaded 由文件系统派生。
    download_progress: Arc<Mutex<HashMap<String, f32>>>,
    /// Cleanup service dependency. 生产组合根使用默认实现；集成测试可注入
    /// 指定 reasoning transport，但业务层仍只持有 CleanupService 抽象边界。
    cleanup: Arc<CleanupService>,
    desktop_capability: Option<Arc<String>>,
    thumbnail_slots: Arc<Semaphore>,
    thumbnail_session_slots: Arc<Mutex<HashMap<String, Weak<Semaphore>>>>,
}

/// 唯一 HTTP 组合根。`ApplicationServices` 只在这里构造一次；`AppState` 只作为
/// 进程级组合输入，不作为 router state 或 handler-facing service locator。
#[derive(Clone)]
pub struct HttpState {
    services: crate::application::ApplicationServices,
    desktop_capability: Option<Arc<String>>,
    thumbnail_slots: Arc<Semaphore>,
    thumbnail_session_slots: Arc<Mutex<HashMap<String, Weak<Semaphore>>>>,
}

impl HttpState {
    pub(crate) fn new(app: AppState) -> Self {
        let catalog = Arc::new(default_catalog());
        let installed: Arc<dyn Fn(&crate::application::ManifestEntry) -> bool + Send + Sync> =
            match app.runtime_factory.clone() {
                Some(factory) => Arc::new(move |entry| (factory.installed)(&entry.id)),
                None => Arc::new(crate::model_runtime::is_installed),
            };
        let builder: Arc<crate::application::ModelRuntimeBuilder> =
            match app.runtime_factory.clone() {
                Some(factory) => Arc::new(move |entry, _| {
                    (factory.build)(&entry.id).map_err(|error| {
                        crate::application::ModelRuntimeBuildError::Internal(error.to_string())
                    })
                }),
                None => Arc::new(crate::model_runtime::build_runtime_for_service),
            };
        let services = crate::application::ApplicationServices::new_with_cleanup(
            Arc::clone(&app.auth),
            Arc::clone(&app.session_mutation_locks),
            Arc::clone(&app.injection_plan_consumed),
            catalog,
            Arc::clone(&app.registry),
            Arc::clone(&app.settings),
            Arc::clone(&app.download_progress),
            installed,
            Arc::clone(&app.gate),
            builder,
            crate::model_runtime::backend_switching_enabled(),
            Arc::clone(&app.normalizer),
            Arc::clone(&app.cleanup),
        );
        Self {
            desktop_capability: app.desktop_capability.clone(),
            thumbnail_slots: Arc::clone(&app.thumbnail_slots),
            thumbnail_session_slots: Arc::clone(&app.thumbnail_session_slots),
            services,
        }
    }

    #[cfg(test)]
    pub(crate) fn services(&self) -> &crate::application::ApplicationServices {
        &self.services
    }

    pub(crate) fn auth_service(&self) -> &Arc<crate::application::AuthService> {
        &self.services.auth
    }

    pub(crate) fn account_service(&self) -> &Arc<crate::application::AccountService> {
        &self.services.accounts
    }

    pub(crate) fn token_service(&self) -> &Arc<crate::application::TokenService> {
        &self.services.tokens
    }

    pub(crate) fn sessions(&self) -> &Arc<crate::application::SessionService> {
        &self.services.sessions
    }

    pub(crate) fn transcription_service(&self) -> &Arc<crate::application::TranscriptionService> {
        &self.services.transcription
    }

    pub(crate) fn export_service(&self) -> &Arc<crate::application::ExportService> {
        &self.services.export
    }

    pub(crate) fn model_service(&self) -> &Arc<crate::application::ModelService> {
        &self.services.models
    }

    pub(crate) fn dictionary_service(&self) -> &Arc<crate::application::DictionaryService> {
        &self.services.dictionary
    }

    pub(crate) fn thumbnail_slots(&self) -> &Arc<Semaphore> {
        &self.thumbnail_slots
    }

    pub(crate) fn thumbnail_session_slots(&self, id: &str) -> Arc<Semaphore> {
        let mut slots = self
            .thumbnail_session_slots
            .lock()
            .expect("thumbnail session slots mutex");
        if let Some(existing) = slots.get(id).and_then(Weak::upgrade) {
            return existing;
        }
        let created = Arc::new(Semaphore::new(1));
        slots.insert(id.to_owned(), Arc::downgrade(&created));
        created
    }
}

#[cfg(test)]
mod thumbnail_budget_tests;

impl AppState {
    pub fn with_desktop_capability(mut self, key: String) -> Self {
        self.desktop_capability = Some(Arc::new(key));
        self
    }

    pub fn with_runtime_factory(mut self, factory: RuntimeFactory) -> Self {
        self.runtime_factory = Some(factory);
        self
    }

    pub(crate) async fn stop_runtime_for_lifecycle(&self) -> std::io::Result<()> {
        if let Some(active) = self.registry.active().await {
            active.stop().await?;
        }
        self.registry.clear().await;
        let report = self.registry.reap_shutdown_children().await;
        if report.unresolved_records != 0 {
            return Err(std::io::Error::other(
                "subprocess ownership/exit not verified",
            ));
        }
        Ok(())
    }

    /// 生产构造：默认 runtime 句柄（空 registry + 新调度器 + normalizer 用
    /// `FFMPEG_PATH` env 或 PATH `ffmpeg`——生产 app bundle 内为 `Resources/ffmpeg`，
    /// M6 打包期内置；此处 env/PATH 探测便于 dev/test）。
    pub fn new(auth: Arc<Auth>, data_dir: PathBuf) -> Self {
        let storage = Arc::new(Storage::new(auth.crypto_arc()));
        let registry = Arc::new(SidecarRegistry::new_with_orphan_dir(
            data_dir.join("sidecars"),
        ));
        let gate = Arc::new(RuntimeOperationGate::new_for_composition_root());
        let normalizer = Arc::new(AudioNormalizer::new(default_ffmpeg_path()));
        let settings = Arc::new(ModelSettings::new(data_dir));
        Self {
            auth,
            storage,
            registry,
            gate,
            normalizer,
            runtime_factory: None,
            settings,
            session_mutation_locks: Arc::new(Mutex::new(HashMap::new())),
            injection_plan_consumed: Arc::new(Mutex::new(HashSet::new())),
            download_progress: Arc::new(Mutex::new(HashMap::new())),
            cleanup: Arc::new(CleanupService::default()),
            desktop_capability: None,
            thumbnail_slots: Arc::new(Semaphore::new(2)),
            thumbnail_session_slots: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 测试/注入构造：显式 runtime 句柄（mock 注册表 + 真实 ffmpeg normalizer 等）。
    /// `storage` 仍由 `auth.crypto_arc()` 内部构造，与 `new` 一致——共享同一份编排态。
    /// catalog 由 `HttpState` 组合进 `ModelService`，测试仍使用内嵌清单中的真实 id。
    pub fn with_runtime_handles(
        auth: Arc<Auth>,
        registry: Arc<SidecarRegistry>,
        gate: Arc<RuntimeOperationGate>,
        normalizer: Arc<AudioNormalizer>,
        data_dir: PathBuf,
    ) -> Self {
        Self::with_runtime_handles_and_cleanup(
            auth,
            registry,
            gate,
            normalizer,
            data_dir,
            Arc::new(CleanupService::default()),
        )
    }

    /// 测试/组合根构造：允许显式注入 cleanup service，以便 pipeline 集成测试使用
    /// 本地 mock reasoning endpoint；生产调用仍使用 `with_runtime_handles`。
    pub fn with_runtime_handles_and_cleanup(
        auth: Arc<Auth>,
        registry: Arc<SidecarRegistry>,
        gate: Arc<RuntimeOperationGate>,
        normalizer: Arc<AudioNormalizer>,
        data_dir: PathBuf,
        cleanup: Arc<CleanupService>,
    ) -> Self {
        let storage = Arc::new(Storage::new(auth.crypto_arc()));
        let settings = Arc::new(ModelSettings::new(data_dir));
        Self {
            auth,
            storage,
            registry,
            gate,
            normalizer,
            runtime_factory: None,
            settings,
            session_mutation_locks: Arc::new(Mutex::new(HashMap::new())),
            injection_plan_consumed: Arc::new(Mutex::new(HashSet::new())),
            download_progress: Arc::new(Mutex::new(HashMap::new())),
            cleanup,
            desktop_capability: None,
            thumbnail_slots: Arc::new(Semaphore::new(2)),
            thumbnail_session_slots: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// ST-M4.4：标记会话的注入计划已被消费。原子 check+insert：首读返回 `true`，
    /// 再次读返回 `false`（handler 据此返回 410 Gone）。进程内、不持久化（重启重置）。
    pub fn mark_injection_plan_consumed(&self, session_id: &str) -> bool {
        self.injection_plan_consumed
            .lock()
            .expect("injection_plan_consumed mutex")
            .insert(session_id.to_owned())
    }

    /// 取指定会话的短生命周期互斥锁。编辑与删除均在取得行前持有它，故同一会话上
    /// 不会出现「PUT 读行→DELETE 删目录→PUT 写回孤儿文件」的交错。
    pub fn session_mutation_lock(&self, id: &str) -> Arc<Mutex<()>> {
        let mut locks = self
            .session_mutation_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(lock) = locks.get(id).and_then(Weak::upgrade) {
            return lock;
        }
        locks.retain(|_, lock| lock.strong_count() > 0);
        let lock = Arc::new(Mutex::new(()));
        locks.insert(id.to_owned(), Arc::downgrade(&lock));
        lock
    }

    pub(crate) fn reconcile_storage_for_lifecycle(
        &self,
    ) -> Result<(), crate::account::AccountError> {
        self.storage.reconcile().map(|_| ())
    }

    pub(crate) async fn reap_runtime_orphans_for_lifecycle(&self) -> seasnail_runtime::ReapReport {
        self.registry.reap_orphans().await
    }
}

/// ffmpeg 路径解析：`FFMPEG_PATH` env → PATH `ffmpeg`。
/// 生产应指向 app bundle `Resources/ffmpeg`（M6 打包期内置静态二进制）。
fn default_ffmpeg_path() -> PathBuf {
    if let Ok(p) = std::env::var("FFMPEG_PATH") {
        return PathBuf::from(p);
    }
    // `.app` 内必须使用随 Resources 分发的 ffmpeg；即使该待办资源尚未补齐，也不
    // 回退到开发机 PATH，避免开发/发行行为悄然不同。
    if let Some(path) = crate::app_resources::bundled_resource("ffmpeg") {
        return path;
    }
    PathBuf::from("ffmpeg")
}

/// 模型目录：由内嵌清单 `resources/models.json` 构造（编译期 `include_str!`）。
/// 畸形清单 = 内嵌资源损坏（开发 bug），无运行期缓解 → expect 早早失败。
fn default_catalog() -> ModelCatalog {
    ModelCatalog::from_manifest_str(include_str!(concat!(env!("OUT_DIR"), "/models.json")))
        .expect("bundled models.json must be valid")
}

/// `/api/v1` 子路由聚合。各资源子模块 `routes()` 返回相对路径子路由，merge 后挂
/// `fallback` → 统一 404 `{error:{code,message}}`（ST-M1.6）。
pub fn router() -> Router<HttpState> {
    Router::new()
        .merge(auth::routes())
        .merge(accounts::routes())
        .merge(tokens::routes())
        .merge(sessions::routes())
        .merge(models::routes())
        .merge(reasoning::routes())
        .merge(dictionary::routes())
        .merge(export::routes())
        .fallback(not_found)
}

/// 未匹配的 OpenAPI 路由 → 404 `not_found`（统一错误响应）。
async fn not_found() -> AppError {
    AppError::NotFound("route not found".into())
}

// ── 鉴权提取器 ──────────────────────────────────────────────────────────────────

/// 已鉴权调用方提取器：从 `Authorization: Bearer ss_live_xxx` 头 verify 出 `Caller`。
///
/// 失败映射（verify 路径专用，账户/token 不存在统一 401 不泄露）：
/// - 缺头 / 非 `Bearer` → 401 `unauthorized`。
/// - `InvalidToken` / `AccountNotFound` / `TokenNotFound` → 401（经 [`verify_err_to_apperr`]）。
/// - `NotUnlocked` / `KeychainMissing` → 423 `locked`。
/// - 其余 → `From<AccountError>` 默认。
pub struct AuthedCaller(pub CallerContext);

#[async_trait]
impl<S> FromRequestParts<S> for AuthedCaller
where
    S: Send + Sync,
    HttpState: axum::extract::FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let state: HttpState = axum::extract::FromRef::from_ref(state);
        let bearer = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .ok_or_else(|| {
                AppError::Unauthorized("missing or invalid Authorization header".into())
            })?;
        let caller = state
            .auth_service()
            .authenticate_and_bind(bearer)
            .map_err(AppError::from)?;
        Ok(AuthedCaller(caller))
    }
}

// 避免未使用 import 警告（State/Request 在 handler 子模块用，此处仅提取器用 FromRequestParts）。

/// handler 内粗粒度 scope 守门：caller 须持 `scope`，否则 403 `insufficient_scope`。
///
/// 细粒度授权（如 `enforce_grantable` 拒授 write/manage 给第三方）仍在编排层
///（`Auth::issue_token` 等内部）做。本函数仅作端点 `x-required-scope` 的守门。
pub fn require_scope(caller: &CallerContext, scope: &str) -> Result<(), AppError> {
    if caller.has_scope(scope) {
        Ok(())
    } else {
        Err(AppError::InsufficientScope(format!(
            "requires scope: {scope}"
        )))
    }
}

/// handler 内 root-only 守门（ST-M4.4）：caller 须为 root token（原生 DaemonClient 持有），
/// 否则 403 `insufficient_scope`。第三方 token 恒 `is_root=false`（`enforce_grantable`），
/// 故无法通过；WebView 另经 `validate_api_request` 白名单隔离（端点不在白名单）。
pub fn require_root(caller: &CallerContext) -> Result<(), AppError> {
    if caller.is_root() {
        Ok(())
    } else {
        Err(AppError::InsufficientScope("requires root token".into()))
    }
}
