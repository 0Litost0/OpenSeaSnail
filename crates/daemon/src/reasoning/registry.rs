//! Provider registry 与 Chat Completions adapters（ST-M3.4）。
//!
//! registry 是**唯一**允许按 provider type 分支的位置；cleanup 等业务层只见统一
//! 请求/响应契约。adapter 负责最终 `/chat/completions` 路径、output-token 字段、
//! structured-output 与 temperature 能力、请求 body 与响应文本提取、HTTP 状态归一。
//!
//! MVP 决策（技术设计「MVP provider 类型」）：
//! - `openai`：`max_completion_tokens` + strict JSON schema + temperature；
//! - `openai_compatible_cloud`：`max_tokens` + `json_object` + temperature，并显式关闭
//!   provider 深度思考以控制实时 cleanup 延迟；
//! - 两个 self-hosted：最小方言，只发送 `model/messages/stream:false`，不发
//!   token cap、structured output、temperature 或 thinking/reasoning 参数；
//! - 正式请求不做 `/responses` 与 `/chat/completions` 运行时试探，也不在 400 后
//!   移除参数重试；兼容性问题由 Probe/Prompt Studio 暴露。
//!
//! 另承载 known-native host guard（避免原生协议服务被误配为 generic endpoint 的
//! UX guard，不替代 IP/TLS/credential 安全检查）。

use seasnail_storage::ProviderType;
use serde_json::{json, Value};
use url::Url;

use super::{OutputContract, ReasoningError, ReasoningErrorKind};

/// 已知原生协议服务 hostname。判断必须精确相等或 dot-suffix，禁止 substring。
///
/// 收录范围：MVP 不提供专用 adapter、且会被 generic OpenAI-compatible 配置误用的
/// 原生协议服务（Anthropic / Gemini / Tinfoil attested SDK）。Groq 等服务提供真实的
/// OpenAI-compatible 端点，不在此列。
const KNOWN_NATIVE_HOSTS: &[&str] = &[
    "api.anthropic.com",
    "generativelanguage.googleapis.com",
    "tinfoil.sh",
];

/// `host == known || host.ends_with("." + known)`。调用方传入的 host 须是 URL
/// parser 已完成 IDNA canonicalization 的形式。
pub fn is_known_native_host(host: &str) -> bool {
    KNOWN_NATIVE_HOSTS.iter().any(|known| {
        host == *known
            || host
                .strip_suffix(known)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

/// MVP 全局 output token 封顶（设计：16,384）。
pub const GLOBAL_OUTPUT_TOKEN_CAP: u32 = 16_384;

/// 估算下限：避免极小输入得到病态 token cap。
const MIN_RESERVED_OUTPUT_TOKENS: u32 = 64;

/// output-token 参数方言：不同厂商的字段名与该 adapter 声明的安全上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenDialect {
    pub field: &'static str,
    pub safety_cap: u32,
}

/// 结构化输出能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredOutputMode {
    /// OpenAI strict JSON schema。
    JsonSchema,
    /// 广泛兼容的 `{"type": "json_object"}`。
    JsonObject,
}

/// adapter 能力声明。cloud 类 adapter 必须声明 token dialect（不变量由
/// `cloud_adapters_always_declare_token_dialect` 测试固定），缺失即不得启用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterCapabilities {
    pub token_dialect: Option<TokenDialect>,
    pub structured_output: Option<StructuredOutputMode>,
    pub supports_temperature: bool,
    /// 是否使用 OpenAI-compatible cloud 的扩展字段显式关闭深度思考。
    pub disables_thinking: bool,
}

/// adapter 构造请求所需的全部输入。
pub struct AdapterRequest<'a> {
    pub model: &'a str,
    pub system_prompt: &'a str,
    pub user_json: &'a str,
    pub output_contract: OutputContract,
}

/// 保留正文所需的 output token 估算：cleanup 输出长度接近输入，按 UTF-8 字节
/// `ceil(bytes/2)` 高估（英文约 2x、中文约 1.5x），下限 64。只用于 token 预留，
/// 不截断任何内容。
pub fn estimate_reserved_output_tokens(input_utf8_bytes: usize) -> u32 {
    let estimated = (input_utf8_bytes as u64).saturating_add(1) / 2;
    let estimated = u32::try_from(estimated).unwrap_or(u32::MAX);
    estimated.clamp(MIN_RESERVED_OUTPUT_TOKENS, GLOBAL_OUTPUT_TOKEN_CAP)
}

/// `output_token_limit = min(adapter 安全上限, 输入估算, 全局 16,384)`。
pub fn output_token_limit(dialect: &TokenDialect, input_utf8_bytes: usize) -> u32 {
    estimate_reserved_output_tokens(input_utf8_bytes)
        .min(dialect.safety_cap)
        .min(GLOBAL_OUTPUT_TOKEN_CAP)
}

/// cleanup 结构化输出 schema（与设计「Correction schema」逐字段一致）。
fn cleanup_json_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["cleaned_text", "corrections"],
        "properties": {
            "cleaned_text": { "type": "string" },
            "corrections": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["original_text", "corrected_text", "kind"],
                    "properties": {
                        "original_text": { "type": "string" },
                        "corrected_text": { "type": "string" },
                        "kind": {
                            "type": "string",
                            "enum": ["phonetic", "proper_noun", "other_asr"]
                        }
                    }
                }
            }
        }
    })
}

/// Provider 协议适配器。四个 MVP adapter 都是 Chat Completions 方言，差异由
/// 能力声明驱动；新增原生协议 provider 时新增 adapter，不修改业务层。
pub trait ProviderAdapter: Send + Sync {
    fn capabilities(&self) -> AdapterCapabilities;

    /// 最终 chat completions URL：canonical base + `/chat/completions`。
    fn chat_completions_url(&self, canonical_base: &Url) -> String {
        format!(
            "{}/chat/completions",
            canonical_base.as_str().trim_end_matches('/')
        )
    }

    /// 请求 body：`model/messages/stream:false` 为最小方言，其余字段由能力驱动。
    /// `JsonObject` 契约且 adapter 有结构化能力时发送对应 `response_format`。
    fn build_request_body(&self, request: &AdapterRequest<'_>) -> Value {
        let capabilities = self.capabilities();
        let mut body = json!({
            "model": request.model,
            "stream": false,
            "messages": [
                { "role": "system", "content": request.system_prompt },
                { "role": "user", "content": request.user_json },
            ],
        });
        if capabilities.supports_temperature {
            // cleanup 追求保真：低温输出。
            body["temperature"] = json!(0.0);
        }
        if capabilities.disables_thinking {
            // 实时 cleanup 优先低延迟；该扩展只由明确声明能力的 adapter 发送。
            body["thinking"] = json!({ "type": "disabled" });
        }
        if let Some(dialect) = &capabilities.token_dialect {
            let input_bytes = request.system_prompt.len() + request.user_json.len();
            body[dialect.field] = json!(output_token_limit(dialect, input_bytes));
        }
        if request.output_contract == OutputContract::JsonObject {
            match capabilities.structured_output {
                Some(StructuredOutputMode::JsonSchema) => {
                    body["response_format"] = json!({
                        "type": "json_schema",
                        "json_schema": {
                            "name": "seasnail_cleanup",
                            "strict": true,
                            "schema": cleanup_json_schema(),
                        }
                    });
                }
                Some(StructuredOutputMode::JsonObject) => {
                    body["response_format"] = json!({ "type": "json_object" });
                }
                None => {}
            }
        }
        body
    }

    /// 从 Chat Completions 响应提取正文。`finish_reason=length`（provider token
    /// limit 截断）按非法响应处理，绝不接受半截 JSON。
    fn extract_content(&self, body: &Value) -> Result<String, ReasoningError> {
        let choice = body
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .ok_or_else(|| ReasoningError::response_invalid("response has no choices"))?;
        if choice.get("finish_reason").and_then(Value::as_str) == Some("length") {
            return Err(ReasoningError::response_invalid(
                "provider truncated the response at its token limit",
            ));
        }
        let content = choice
            .pointer("/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| ReasoningError::response_invalid("response has no message content"))?;
        Ok(content.to_owned())
    }

    /// Authorization header：MVP 四类 provider 均为 Bearer；未来原生协议 adapter
    /// 可覆盖（如 `x-api-key`）。secret 构造期已保证 header-safe。
    fn authorization_header(
        &self,
        secret: &seasnail_crypto::CredentialSecret,
    ) -> reqwest::header::HeaderValue {
        super::credential::authorization_header(secret)
    }

    /// HTTP 状态归一：401/403 → auth、429 → rate limit、5xx → server；
    /// 其余 4xx 视为 provider 拒绝请求形状/模型，归为 configuration。
    fn map_http_status(&self, status: u16) -> ReasoningErrorKind {
        match status {
            401 | 403 => ReasoningErrorKind::HttpAuth,
            429 => ReasoningErrorKind::HttpRateLimit,
            500..=599 => ReasoningErrorKind::HttpServer,
            _ => ReasoningErrorKind::Configuration,
        }
    }
}

struct OpenAiAdapter;
struct OpenAiCompatibleCloudAdapter;
struct SelfHostedPublicAdapter;
struct SelfHostedPrivateAdapter;

impl ProviderAdapter for OpenAiAdapter {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            token_dialect: Some(TokenDialect {
                field: "max_completion_tokens",
                safety_cap: GLOBAL_OUTPUT_TOKEN_CAP,
            }),
            structured_output: Some(StructuredOutputMode::JsonSchema),
            supports_temperature: true,
            disables_thinking: false,
        }
    }
}

impl ProviderAdapter for OpenAiCompatibleCloudAdapter {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            token_dialect: Some(TokenDialect {
                field: "max_tokens",
                safety_cap: GLOBAL_OUTPUT_TOKEN_CAP,
            }),
            structured_output: Some(StructuredOutputMode::JsonObject),
            supports_temperature: true,
            disables_thinking: true,
        }
    }
}

impl ProviderAdapter for SelfHostedPublicAdapter {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            token_dialect: None,
            structured_output: None,
            supports_temperature: false,
            disables_thinking: false,
        }
    }
}

impl ProviderAdapter for SelfHostedPrivateAdapter {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            token_dialect: None,
            structured_output: None,
            supports_temperature: false,
            disables_thinking: false,
        }
    }
}

/// Provider registry：唯一按 provider type 分支的位置。
pub struct ProviderRegistry;

impl ProviderRegistry {
    pub fn adapter(provider_type: ProviderType) -> &'static dyn ProviderAdapter {
        match provider_type {
            ProviderType::OpenAi => &OpenAiAdapter,
            ProviderType::OpenAiCompatibleCloud => &OpenAiCompatibleCloudAdapter,
            ProviderType::SelfHostedPublic => &SelfHostedPublicAdapter,
            ProviderType::SelfHostedPrivate => &SelfHostedPrivateAdapter,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cleanup_request(model: &'static str) -> AdapterRequest<'static> {
        AdapterRequest {
            model,
            system_prompt: "system prompt",
            user_json: "{\"transcript\":\"x\"}",
            output_contract: OutputContract::JsonObject,
        }
    }

    #[test]
    fn known_native_host_requires_exact_or_dot_suffix() {
        assert!(is_known_native_host("api.anthropic.com"));
        assert!(is_known_native_host("sub.api.anthropic.com"));
        assert!(is_known_native_host("tinfoil.sh"));

        // substring 形态不得命中。
        assert!(!is_known_native_host("api.anthropic.com.evil.example"));
        assert!(!is_known_native_host("notapi.anthropic.com"));
        assert!(!is_known_native_host("anthropic.com"));
        assert!(!is_known_native_host(
            "generativelanguage.googleapis.com.evil.example"
        ));
        // 提供真实 OpenAI-compatible 端点的服务不得被误伤。
        assert!(!is_known_native_host("api.groq.com"));
        assert!(!is_known_native_host("api.openai.com"));
    }

    #[test]
    fn cloud_adapters_always_declare_token_dialect() {
        // 设计不变量：缺少已知 token dialect 的 cloud 配置不得启用。
        for provider_type in [ProviderType::OpenAi, ProviderType::OpenAiCompatibleCloud] {
            assert!(ProviderRegistry::adapter(provider_type)
                .capabilities()
                .token_dialect
                .is_some());
        }
    }

    #[test]
    fn chat_completions_url_appends_resource_path() {
        let adapter = ProviderRegistry::adapter(ProviderType::OpenAi);
        let base = Url::parse("https://api.openai.com/v1").unwrap();
        assert_eq!(
            adapter.chat_completions_url(&base),
            "https://api.openai.com/v1/chat/completions"
        );
        let base = Url::parse("http://10.0.0.2:8080/v1").unwrap();
        assert_eq!(
            adapter.chat_completions_url(&base),
            "http://10.0.0.2:8080/v1/chat/completions"
        );
        // 自定义子路径保留。
        let base = Url::parse("https://gateway.example/api/llm/v1").unwrap();
        assert_eq!(
            adapter.chat_completions_url(&base),
            "https://gateway.example/api/llm/v1/chat/completions"
        );
    }

    #[test]
    fn openai_request_snapshot() {
        let adapter = ProviderRegistry::adapter(ProviderType::OpenAi);
        let body = adapter.build_request_body(&cleanup_request("gpt-5-mini"));
        assert_eq!(body["model"], "gpt-5-mini");
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "system prompt");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "{\"transcript\":\"x\"}");
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(
            body["max_completion_tokens"],
            output_token_limit(
                &TokenDialect {
                    field: "max_completion_tokens",
                    safety_cap: GLOBAL_OUTPUT_TOKEN_CAP,
                },
                "system prompt".len() + "{\"transcript\":\"x\"}".len()
            )
        );
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("thinking").is_none());
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        assert_eq!(
            body["response_format"]["json_schema"]["schema"],
            cleanup_json_schema()
        );
    }

    #[test]
    fn compatible_cloud_request_snapshot() {
        let adapter = ProviderRegistry::adapter(ProviderType::OpenAiCompatibleCloud);
        let body = adapter.build_request_body(&cleanup_request("qwen3-32b"));
        assert_eq!(body["model"], "qwen3-32b");
        assert_eq!(body["stream"], false);
        assert_eq!(body["temperature"], 0.0);
        assert!(body.get("max_completion_tokens").is_none());
        assert!(body["max_tokens"].is_u64());
        assert_eq!(body["thinking"], json!({ "type": "disabled" }));
        assert_eq!(body["response_format"], json!({ "type": "json_object" }));
    }

    #[test]
    fn self_hosted_request_is_minimal_dialect() {
        for provider_type in [
            ProviderType::SelfHostedPublic,
            ProviderType::SelfHostedPrivate,
        ] {
            let adapter = ProviderRegistry::adapter(provider_type);
            let body = adapter.build_request_body(&cleanup_request("local-model"));
            let object = body.as_object().unwrap();
            let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                ["messages", "model", "stream"],
                "self-hosted dialect must stay minimal"
            );
            assert_eq!(body["stream"], false);
            assert!(body.get("thinking").is_none());
        }
    }

    #[test]
    fn freeform_contract_never_requests_structured_output() {
        for provider_type in [
            ProviderType::OpenAi,
            ProviderType::OpenAiCompatibleCloud,
            ProviderType::SelfHostedPublic,
            ProviderType::SelfHostedPrivate,
        ] {
            let adapter = ProviderRegistry::adapter(provider_type);
            let body = adapter.build_request_body(&AdapterRequest {
                model: "m",
                system_prompt: "s",
                user_json: "u",
                output_contract: OutputContract::FreeformText,
            });
            assert!(body.get("response_format").is_none());
            if provider_type == ProviderType::OpenAiCompatibleCloud {
                assert_eq!(body["thinking"], json!({ "type": "disabled" }));
            } else {
                assert!(body.get("thinking").is_none());
            }
        }
    }

    #[test]
    fn output_token_limit_respects_estimate_cap_and_global_cap() {
        let dialect = TokenDialect {
            field: "max_tokens",
            safety_cap: 8_192,
        };
        // 小输入：估算下限 64。
        assert_eq!(output_token_limit(&dialect, 10), 64);
        // 估算生效：1000 bytes → 500。
        assert_eq!(output_token_limit(&dialect, 1_000), 500);
        // adapter 安全上限生效：100 KiB → 50_000 估算被 8_192 截断。
        assert_eq!(output_token_limit(&dialect, 100 * 1024), 8_192);
        // 全局封顶 16_384 永远生效。
        let uncapped = TokenDialect {
            field: "max_tokens",
            safety_cap: u32::MAX,
        };
        assert_eq!(
            output_token_limit(&uncapped, 1024 * 1024),
            GLOBAL_OUTPUT_TOKEN_CAP
        );
    }

    #[test]
    fn content_extraction_and_truncation_rejection() {
        let adapter = ProviderRegistry::adapter(ProviderType::OpenAi);
        let ok = json!({
            "choices": [{
                "message": { "role": "assistant", "content": "{\"cleaned_text\":\"hi\",\"corrections\":[]}" },
                "finish_reason": "stop"
            }]
        });
        assert!(adapter
            .extract_content(&ok)
            .unwrap()
            .contains("cleaned_text"));

        // finish_reason=length：provider token limit 截断，按非法响应处理。
        let truncated = json!({
            "choices": [{ "message": { "content": "{\"cleaned_text\":\"" }, "finish_reason": "length" }]
        });
        assert_eq!(
            adapter.extract_content(&truncated).unwrap_err().kind(),
            ReasoningErrorKind::ResponseInvalid
        );

        for bad in [
            json!({}),
            json!({ "choices": [] }),
            json!({ "choices": [{ "message": {}, "finish_reason": "stop" }] }),
            json!({ "choices": [{ "message": { "content": null }, "finish_reason": "stop" }] }),
        ] {
            assert_eq!(
                adapter.extract_content(&bad).unwrap_err().kind(),
                ReasoningErrorKind::ResponseInvalid
            );
        }
    }

    #[test]
    fn authorization_header_defaults_to_bearer_for_all_adapters() {
        let secret = seasnail_crypto::CredentialSecret::new(b"sk-test".to_vec()).unwrap();
        for provider_type in [
            ProviderType::OpenAi,
            ProviderType::OpenAiCompatibleCloud,
            ProviderType::SelfHostedPublic,
            ProviderType::SelfHostedPrivate,
        ] {
            let header = ProviderRegistry::adapter(provider_type).authorization_header(&secret);
            assert_eq!(header.to_str().unwrap(), "Bearer sk-test");
        }
    }

    #[test]
    fn http_status_mapping_is_stable() {
        let adapter = ProviderRegistry::adapter(ProviderType::SelfHostedPublic);
        assert_eq!(adapter.map_http_status(401), ReasoningErrorKind::HttpAuth);
        assert_eq!(adapter.map_http_status(403), ReasoningErrorKind::HttpAuth);
        assert_eq!(
            adapter.map_http_status(429),
            ReasoningErrorKind::HttpRateLimit
        );
        assert_eq!(adapter.map_http_status(500), ReasoningErrorKind::HttpServer);
        assert_eq!(adapter.map_http_status(503), ReasoningErrorKind::HttpServer);
        assert_eq!(
            adapter.map_http_status(400),
            ReasoningErrorKind::Configuration
        );
        assert_eq!(
            adapter.map_http_status(404),
            ReasoningErrorKind::Configuration
        );
        assert_eq!(
            adapter.map_http_status(422),
            ReasoningErrorKind::Configuration
        );
    }
}
