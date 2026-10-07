//! Provider 配置、cleanup settings 与显式 Probe 路由（M7.2）。
//!
//! 所有配置端点均 root-only；credential 不经通用 JSON API 返回，只能通过
//! Tauri 直连的 internal write-only 路由写入或删除。响应只包含非 secret 配置和脱敏状态。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::account::{CleanupSettingsView, CredentialState, ProviderConfigView};
use crate::api::{require_root, AuthedCaller, HttpState};
use crate::error::AppError;
use seasnail_storage::{ProviderConfigInput, ProviderType};

#[derive(Debug, Serialize)]
struct ProviderKindDto {
    provider_type: &'static str,
    requires_credential: bool,
}

#[derive(Debug, Serialize)]
struct ProviderConfigDto {
    id: String,
    name: String,
    provider_type: String,
    endpoint: String,
    model: String,
    created_at: String,
    updated_at: String,
    credential_state: &'static str,
}

#[derive(Debug, Deserialize)]
struct ProviderConfigRequest {
    name: String,
    provider_type: String,
    endpoint: String,
    model: String,
}

#[derive(Serialize)]
struct CleanupSettingsDto {
    enabled: bool,
    selected_provider_config_id: Option<String>,
    custom_prompt: Option<String>,
    default_prompt: &'static str,
    protocol_prompt: &'static str,
    updated_at: String,
    selected_credential_state: Option<&'static str>,
}

#[derive(Debug, Deserialize)]
struct CleanupSettingsRequest {
    enabled: bool,
    selected_provider_config_id: Option<String>,
    custom_prompt: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProbeDto {
    model: String,
    elapsed_ms: u64,
    token_cap: &'static str,
    structured_output: bool,
}

#[derive(Deserialize)]
struct CleanupTestRequest {
    provider_config_id: String,
    text: String,
    prompt_draft: Option<String>,
}

#[derive(Serialize)]
struct CleanupTestCorrectionDto {
    original_text: String,
    corrected_text: String,
    kind: &'static str,
}

#[derive(Serialize)]
struct CleanupTestDto {
    cleaned_text: String,
    corrections: Vec<CleanupTestCorrectionDto>,
    elapsed_ms: u64,
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum CredentialWriteRequest {
    Credential { credential: String },
    NoAuth,
}

#[derive(Debug, Serialize)]
struct CredentialStateDto {
    credential_state: &'static str,
}

fn credential_state(state: CredentialState) -> &'static str {
    match state {
        CredentialState::NotRequired => "not_required",
        CredentialState::Missing => "missing",
        CredentialState::Bound => "bound",
        CredentialState::Stale => "stale",
    }
}

fn config_dto(config: ProviderConfigView) -> ProviderConfigDto {
    ProviderConfigDto {
        id: config.id,
        name: config.name,
        provider_type: config.provider_type,
        endpoint: config.endpoint,
        model: config.model,
        created_at: crate::api::dto::epoch_to_rfc3339(config.created_at),
        updated_at: crate::api::dto::epoch_to_rfc3339(config.updated_at),
        credential_state: credential_state(config.credential_state),
    }
}

fn settings_dto(settings: CleanupSettingsView) -> CleanupSettingsDto {
    CleanupSettingsDto {
        enabled: settings.enabled,
        selected_provider_config_id: settings.selected_provider_config_id,
        custom_prompt: settings.custom_prompt,
        default_prompt: crate::cleanup::prompt::DEFAULT_SEMANTIC_PROMPT,
        protocol_prompt: crate::cleanup::prompt::PROTOCOL_PROMPT,
        updated_at: crate::api::dto::epoch_to_rfc3339(settings.updated_at),
        selected_credential_state: settings.selected_credential_state.map(credential_state),
    }
}

fn parse_provider_type(value: &str) -> Result<ProviderType, AppError> {
    ProviderType::parse(value).ok_or_else(|| AppError::BadRequest("invalid provider_type".into()))
}

fn config_input(request: &ProviderConfigRequest) -> Result<ProviderConfigInput<'_>, AppError> {
    Ok(ProviderConfigInput {
        name: &request.name,
        provider_type: parse_provider_type(&request.provider_type)?,
        endpoint: &request.endpoint,
        model: &request.model,
    })
}

async fn list_provider_kinds(
    AuthedCaller(caller): AuthedCaller,
) -> Result<Json<Vec<ProviderKindDto>>, AppError> {
    require_root(&caller)?;
    Ok(Json(
        [
            ProviderType::OpenAi,
            ProviderType::OpenAiCompatibleCloud,
            ProviderType::SelfHostedPublic,
            ProviderType::SelfHostedPrivate,
        ]
        .into_iter()
        .map(|provider_type| ProviderKindDto {
            provider_type: provider_type.as_str(),
            requires_credential: provider_type.requires_credential(),
        })
        .collect(),
    ))
}

async fn list_configs(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
) -> Result<Json<Vec<ProviderConfigDto>>, AppError> {
    Ok(Json(
        state
            .account_service()
            .list_provider_configs(&caller)?
            .into_iter()
            .map(config_dto)
            .collect(),
    ))
}

async fn create_config(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Json(request): Json<ProviderConfigRequest>,
) -> Result<(StatusCode, Json<ProviderConfigDto>), AppError> {
    let config = state
        .account_service()
        .create_provider_config(&caller, config_input(&request)?)
        .await?;
    Ok((StatusCode::CREATED, Json(config_dto(config))))
}

async fn replace_config(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
    Json(request): Json<ProviderConfigRequest>,
) -> Result<Json<ProviderConfigDto>, AppError> {
    let config = state
        .account_service()
        .replace_provider_config(&caller, &id, config_input(&request)?)
        .await?
        .ok_or_else(|| AppError::NotFound("provider config not found".into()))?;
    Ok(Json(config_dto(config)))
}

async fn delete_config(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    if !state
        .account_service()
        .delete_provider_config(&caller, &id)?
    {
        return Err(AppError::NotFound("provider config not found".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn get_settings(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
) -> Result<Json<CleanupSettingsDto>, AppError> {
    Ok(Json(settings_dto(
        state.account_service().cleanup_settings(&caller)?,
    )))
}

async fn put_settings(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Json(request): Json<CleanupSettingsRequest>,
) -> Result<Json<CleanupSettingsDto>, AppError> {
    Ok(Json(settings_dto(
        state.account_service().save_cleanup_settings(
            &caller,
            request.enabled,
            request.selected_provider_config_id.as_deref(),
            request.custom_prompt.as_deref(),
        )?,
    )))
}

async fn probe_config(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
) -> Result<Json<ProbeDto>, AppError> {
    let outcome = state.account_service().probe_provider(&caller, &id).await?;
    Ok(Json(ProbeDto {
        model: outcome.model,
        elapsed_ms: outcome.elapsed_ms,
        token_cap: match outcome.token_cap {
            crate::reasoning::probe::ProbeTokenCap::ServerBounded => "server_bounded",
            crate::reasoning::probe::ProbeTokenCap::ClientOnly => "client_only",
        },
        structured_output: outcome.structured_output,
    }))
}

async fn cleanup_test(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Json(request): Json<CleanupTestRequest>,
) -> Result<Json<CleanupTestDto>, AppError> {
    let CleanupTestRequest {
        provider_config_id,
        text,
        prompt_draft,
    } = request;
    let outcome = state
        .account_service()
        .cleanup_test(&caller, &provider_config_id, text, prompt_draft.as_deref())
        .await?;
    Ok(Json(CleanupTestDto {
        cleaned_text: outcome.cleaned_text,
        corrections: outcome
            .corrections
            .into_iter()
            .map(|correction| CleanupTestCorrectionDto {
                original_text: correction.original_text,
                corrected_text: correction.corrected_text,
                kind: match correction.kind {
                    crate::cleanup::correction::CorrectionKind::Phonetic => "phonetic",
                    crate::cleanup::correction::CorrectionKind::ProperNoun => "proper_noun",
                    crate::cleanup::correction::CorrectionKind::OtherAsr => "other_asr",
                },
            })
            .collect(),
        elapsed_ms: outcome.elapsed_ms,
    }))
}

async fn put_credential(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
    Json(request): Json<CredentialWriteRequest>,
) -> Result<Json<CredentialStateDto>, AppError> {
    let state = match request {
        CredentialWriteRequest::Credential { credential } => {
            state
                .account_service()
                .set_provider_credential(&caller, &id, credential.into_bytes())
                .await?
        }
        CredentialWriteRequest::NoAuth => {
            state
                .account_service()
                .set_provider_no_auth(&caller, &id)
                .await?
        }
    };
    Ok(Json(CredentialStateDto {
        credential_state: credential_state(state),
    }))
}

async fn delete_credential(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
) -> Result<Json<CredentialStateDto>, AppError> {
    let state = state
        .account_service()
        .delete_provider_credential(&caller, &id)?;
    Ok(Json(CredentialStateDto {
        credential_state: credential_state(state),
    }))
}

pub fn routes() -> Router<HttpState> {
    Router::new()
        .route("/reasoning/providers", get(list_provider_kinds))
        .route(
            "/reasoning/provider-configs",
            get(list_configs).post(create_config),
        )
        .route(
            "/reasoning/provider-configs/:id",
            put(replace_config).delete(delete_config),
        )
        .route("/reasoning/provider-configs/:id/probe", post(probe_config))
        .route(
            "/internal/reasoning/provider-configs/:id/credential",
            put(put_credential).delete(delete_credential),
        )
        .route("/cleanup/settings", get(get_settings).put(put_settings))
        .route("/cleanup/test", post(cleanup_test))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Auth, Crypto};
    use crate::api::AppState;
    use seasnail_crypto::{Argon2Params, KeychainStore, MemoryKeychain};
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn state_and_root() -> (
        tempfile::TempDir,
        HttpState,
        crate::application::CallerContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let crypto = Arc::new(
            Crypto::new(
                dir.path().to_path_buf(),
                keychain,
                Argon2Params {
                    m_kib: 8192,
                    t_cost: 1,
                    p_cost: 1,
                },
            )
            .unwrap(),
        );
        let state = HttpState::new(AppState::new(
            Arc::new(Auth::new(crypto)),
            dir.path().to_path_buf(),
        ));
        let token = state
            .auth_service()
            .setup_first_account("alice", "pw")
            .unwrap();
        let root = state
            .auth_service()
            .authenticate_and_bind(&token.secret)
            .unwrap();
        (dir, state, root)
    }

    fn openai_request() -> ProviderConfigRequest {
        ProviderConfigRequest {
            name: "Primary".into(),
            provider_type: "openai".into(),
            endpoint: "https://api.openai.com/v1".into(),
            model: "gpt-test".into(),
        }
    }

    async fn cleanup_endpoint(cleaned_json: &'static str) -> u16 {
        let listener = TcpListener::bind((IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                let Some(header_end) = buffer.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&buffer[..header_end]).to_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buffer.len() >= header_end + 4 + length {
                    break;
                }
            }
            let body = serde_json::json!({
                "choices": [{
                    "message": { "content": cleaned_json },
                    "finish_reason": "stop"
                }]
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        port
    }

    #[tokio::test]
    async fn provider_routes_are_root_only_and_never_serialize_credentials() {
        let (_dir, state, root) = state_and_root();
        let issued = state
            .token_service()
            .issue(&root, "reader", vec!["sessions:read".into()])
            .unwrap();
        let reader = state
            .auth_service()
            .authenticate_and_bind(&issued.secret)
            .unwrap();
        let error = list_configs(State(state.clone()), AuthedCaller(reader))
            .await
            .unwrap_err();
        assert!(matches!(error, AppError::InsufficientScope(_)));

        let Json(kinds) = list_provider_kinds(AuthedCaller(root.clone()))
            .await
            .unwrap();
        assert_eq!(kinds.len(), 4);

        let (_, Json(config)) = create_config(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Json(openai_request()),
        )
        .await
        .unwrap();
        let encoded = serde_json::to_value(&config).unwrap();
        assert_eq!(encoded["credential_state"], "missing");
        assert!(encoded.get("credential").is_none());
        assert!(encoded.get("api_key").is_none());

        let Json(configs) = list_configs(State(state.clone()), AuthedCaller(root.clone()))
            .await
            .unwrap();
        assert_eq!(configs.len(), 1);
        let mut replacement = openai_request();
        replacement.model = "gpt-replacement".into();
        let Json(replaced) = replace_config(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Path(config.id.clone()),
            Json(replacement),
        )
        .await
        .unwrap();
        assert_eq!(replaced.model, "gpt-replacement");

        let Json(settings) = get_settings(State(state.clone()), AuthedCaller(root.clone()))
            .await
            .unwrap();
        assert!(!settings.enabled);
        assert_eq!(
            settings.default_prompt,
            crate::cleanup::prompt::DEFAULT_SEMANTIC_PROMPT
        );
        assert_eq!(
            settings.protocol_prompt,
            crate::cleanup::prompt::PROTOCOL_PROMPT
        );
        let Json(settings) = put_settings(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Json(CleanupSettingsRequest {
                enabled: false,
                selected_provider_config_id: Some(config.id.clone()),
                custom_prompt: Some("Keep the original meaning.".into()),
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            settings.custom_prompt.as_deref(),
            Some("Keep the original meaning.")
        );
        assert_eq!(
            delete_config(State(state), AuthedCaller(root), Path(config.id))
                .await
                .unwrap(),
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test]
    async fn invalid_configuration_is_a_client_error_and_prompt_is_not_silently_ignored() {
        let (_dir, state, root) = state_and_root();
        let mut invalid = openai_request();
        invalid.endpoint = "http://api.openai.com/v1".into();
        let error = create_config(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Json(invalid),
        )
        .await
        .err()
        .expect("invalid provider configuration must be rejected");
        assert!(matches!(error, AppError::BadRequest(_)));

        let error = create_config(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Json(ProviderConfigRequest {
                name: "Public on loopback".into(),
                provider_type: "openai_compatible_self_hosted_public".into(),
                endpoint: "https://127.0.0.1/v1".into(),
                model: "model".into(),
            }),
        )
        .await
        .err()
        .expect("public endpoint resolving to loopback must be rejected before persistence");
        assert!(matches!(
            error,
            AppError::CleanupFailure {
                code: "cleanup_endpoint_rejected",
                ..
            }
        ));

        let error = put_settings(
            State(state),
            AuthedCaller(root),
            Json(CleanupSettingsRequest {
                enabled: false,
                selected_provider_config_id: None,
                custom_prompt: Some(" \n\t".into()),
            }),
        )
        .await
        .err()
        .expect("invalid persisted prompt must be rejected");
        assert!(matches!(error, AppError::BadRequest(_)));
    }

    #[tokio::test]
    async fn probe_exposes_a_stable_redacted_failure_code() {
        let (_dir, state, root) = state_and_root();
        let (_, Json(config)) = create_config(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Json(openai_request()),
        )
        .await
        .unwrap();
        let error = probe_config(State(state), AuthedCaller(root), Path(config.id))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppError::CleanupFailure {
                code: "cleanup_credential_missing",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn cleanup_test_is_root_only_and_does_not_persist_prompt_draft() {
        let (_dir, state, root) = state_and_root();
        let issued = state
            .token_service()
            .issue(&root, "reader", vec!["sessions:read".into()])
            .unwrap();
        let reader = state
            .auth_service()
            .authenticate_and_bind(&issued.secret)
            .unwrap();
        let port = cleanup_endpoint(
            r#"{"cleaned_text":"正确词","corrections":[{"original_text":"错误词","corrected_text":"正确词","kind":"proper_noun"}]}"#,
        )
        .await;
        let (_, Json(config)) = create_config(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Json(ProviderConfigRequest {
                name: "Local".into(),
                provider_type: "openai_compatible_self_hosted_private".into(),
                endpoint: format!("http://127.0.0.1:{port}/v1"),
                model: "local-model".into(),
            }),
        )
        .await
        .unwrap();
        let _ = put_credential(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Path(config.id.clone()),
            Json(CredentialWriteRequest::NoAuth),
        )
        .await
        .unwrap();
        let _ = put_settings(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Json(CleanupSettingsRequest {
                enabled: false,
                selected_provider_config_id: Some(config.id.clone()),
                custom_prompt: Some("PERSISTED_PROMPT_SENTINEL".into()),
            }),
        )
        .await
        .unwrap();

        let error = cleanup_test(
            State(state.clone()),
            AuthedCaller(reader),
            Json(CleanupTestRequest {
                provider_config_id: config.id.clone(),
                text: "错误词".into(),
                prompt_draft: Some("DRAFT_PROMPT_SENTINEL".into()),
            }),
        )
        .await
        .err()
        .expect("third-party cleanup test must be rejected");
        assert!(matches!(error, AppError::InsufficientScope(_)));

        let error = cleanup_test(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Json(CleanupTestRequest {
                provider_config_id: config.id.clone(),
                text: "错误词".into(),
                prompt_draft: Some(" \n\t".into()),
            }),
        )
        .await
        .err()
        .expect("invalid cleanup prompt draft must be rejected");
        assert!(matches!(error, AppError::BadRequest(_)));

        let Json(result) = cleanup_test(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Json(CleanupTestRequest {
                provider_config_id: config.id,
                text: "错误词".into(),
                prompt_draft: Some("DRAFT_PROMPT_SENTINEL".into()),
            }),
        )
        .await
        .unwrap();
        assert_eq!(result.cleaned_text, "正确词");
        assert_eq!(result.corrections.len(), 1);
        assert_eq!(result.corrections[0].kind, "proper_noun");

        let Json(settings) = get_settings(State(state), AuthedCaller(root))
            .await
            .unwrap();
        assert_eq!(
            settings.custom_prompt.as_deref(),
            Some("PERSISTED_PROMPT_SENTINEL")
        );
    }

    #[tokio::test]
    async fn credential_routes_are_write_only_root_only_and_support_explicit_no_auth() {
        let (_dir, state, root) = state_and_root();
        let issued = state
            .token_service()
            .issue(&root, "reader", vec!["sessions:read".into()])
            .unwrap();
        let reader = state
            .auth_service()
            .authenticate_and_bind(&issued.secret)
            .unwrap();

        let (_, Json(cloud)) = create_config(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Json(openai_request()),
        )
        .await
        .unwrap();
        let error = put_credential(
            State(state.clone()),
            AuthedCaller(reader),
            Path(cloud.id.clone()),
            Json(CredentialWriteRequest::Credential {
                credential: "must-not-leak".into(),
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, AppError::InsufficientScope(_)));
        let Json(bound) = put_credential(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Path(cloud.id.clone()),
            Json(CredentialWriteRequest::Credential {
                credential: "must-not-leak".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(bound.credential_state, "bound");
        let Json(missing) = delete_credential(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Path(cloud.id),
        )
        .await
        .unwrap();
        assert_eq!(missing.credential_state, "missing");

        let (_, Json(private)) = create_config(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Json(ProviderConfigRequest {
                name: "Private".into(),
                provider_type: "openai_compatible_self_hosted_private".into(),
                endpoint: "http://127.0.0.1:8080/v1".into(),
                model: "local-compatible".into(),
            }),
        )
        .await
        .unwrap();
        let Json(no_auth) = put_credential(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Path(private.id.clone()),
            Json(CredentialWriteRequest::NoAuth),
        )
        .await
        .unwrap();
        assert_eq!(no_auth.credential_state, "not_required");

        let (_, Json(private_lan)) = create_config(
            State(state.clone()),
            AuthedCaller(root.clone()),
            Json(ProviderConfigRequest {
                name: "Private LAN".into(),
                provider_type: "openai_compatible_self_hosted_private".into(),
                endpoint: "http://10.0.0.8:8080/v1".into(),
                model: "local-compatible".into(),
            }),
        )
        .await
        .unwrap();
        let error = put_credential(
            State(state),
            AuthedCaller(root),
            Path(private_lan.id),
            Json(CredentialWriteRequest::Credential {
                credential: "must-not-cross-private-http".into(),
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            AppError::CleanupFailure {
                code: "cleanup_endpoint_rejected",
                ..
            }
        ));
    }
}
