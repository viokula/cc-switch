use crate::app_config::AppType;
use crate::error::AppError;
use crate::provider::Provider;
use serde_json::{json, Value};

const OPTIMIZE_SYSTEM_PROMPT: &str = r#"你是一个提示词优化器。把用户给出的原始提示词改写成更清晰、可执行的任务描述：

- 保留原始意图、约束、语言和专有名词，不新增事实。
- 补全必要的上下文、目标、输入与期望输出；不确定的信息不要编造。
- 结构紧凑，避免套话、重复和过度解释。
- 只输出改写后的提示词本身，不要加前言、解释、Markdown 代码块或引号。

如果原文已经足够清楚，可以只做轻微润色。"#;

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// 构造一次非流式 Messages 请求；转发器会按供应商协议做格式转换。
pub(crate) fn build_request(model: &str, text: &str) -> Value {
    json!({
        "model": model,
        "max_tokens": 4096,
        "stream": false,
        "system": OPTIMIZE_SYSTEM_PROMPT,
        "messages": [{ "role": "user", "content": text }]
    })
}

/// 从各家响应格式里取出第一段文本。
pub(crate) fn extract_text(body: &Value) -> Option<String> {
    let joined = |parts: Vec<&str>| {
        let text = parts
            .into_iter()
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        (!text.is_empty()).then_some(text)
    };

    // AI Provider Messages
    if let Some(blocks) = body.get("content").and_then(Value::as_array) {
        let texts = blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect();
        if let Some(text) = joined(texts) {
            return Some(text);
        }
    }

    // OpenAI Chat Completions
    if let Some(message) = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
    {
        match message.get("content") {
            Some(Value::String(text)) => {
                if let Some(text) = joined(vec![text.as_str()]) {
                    return Some(text);
                }
            }
            Some(Value::Array(blocks)) => {
                let texts = blocks
                    .iter()
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect();
                if let Some(text) = joined(texts) {
                    return Some(text);
                }
            }
            _ => {}
        }
    }

    // OpenAI Responses
    if let Some(items) = body.get("output").and_then(Value::as_array) {
        let texts = items
            .iter()
            .filter_map(|item| item.get("content").and_then(Value::as_array))
            .flatten()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect();
        if let Some(text) = joined(texts) {
            return Some(text);
        }
    }

    // Gemini Native
    if let Some(parts) = body
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
    {
        let texts = parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect();
        if let Some(text) = joined(texts) {
            return Some(text);
        }
    }

    None
}

/// 供应商配置里的默认模型；提示词优化未指定模型时使用。
pub(crate) fn default_model(app_type: &AppType, provider: &Provider) -> Option<String> {
    let config = &provider.settings_config;
    let from_env = |key: &str| {
        config
            .get("env")
            .and_then(|env| env.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string)
    };
    let direct = |key: &str| {
        config
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string)
    };

    match app_type {
        AppType::Claude | AppType::ClaudeDesktop => from_env("ANTHROPIC_MODEL"),
        AppType::Gemini => from_env("GEMINI_MODEL").or_else(|| direct("model")),
        AppType::Codex | AppType::GrokBuild => direct("model").or_else(|| from_env("OPENAI_MODEL")),
        _ => direct("model").or_else(|| from_env("ANTHROPIC_MODEL")),
    }
}

/// 用指定供应商把提示词改写一次。
pub async fn optimize_prompt(
    db: std::sync::Arc<crate::database::Database>,
    app_handle: tauri::AppHandle,
    app_type: AppType,
    provider: Provider,
    model: Option<String>,
    text: &str,
) -> Result<String, AppError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AppError::InvalidInput("prompt is empty".into()));
    }
    let model = model
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
        .or_else(|| default_model(&app_type, &provider))
        .ok_or_else(|| AppError::InvalidInput("no model configured for prompt optimizer".into()))?;

    let body = build_request(&model, text);
    let server = crate::proxy::server::ProxyServer::new(
        crate::proxy::types::ProxyConfig::default(),
        db,
        Some(app_handle),
    );
    let result = server
        .forward_once(&app_type, &provider, "/v1/messages", &body)
        .await
        .map_err(|error| AppError::Message(format!("prompt optimizer request failed: {error}")))?;

    let status = result.response.status();
    let bytes = result
        .response
        .bytes_with_limit(MAX_RESPONSE_BYTES)
        .await
        .map_err(|error| AppError::Message(format!("prompt optimizer read failed: {error}")))?;
    let value: Option<Value> = serde_json::from_slice(&bytes).ok();
    let raw_text = String::from_utf8_lossy(&bytes);

    if !status.is_success() {
        let detail = value
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .or_else(|| {
                value
                    .as_ref()
                    .and_then(|value| value.get("message"))
                    .and_then(Value::as_str)
            })
            .map(str::to_string)
            .unwrap_or_else(|| {
                raw_text
                    .chars()
                    .take(200)
                    .collect::<String>()
                    .trim()
                    .to_string()
            });
        return Err(AppError::HttpStatus {
            status: status.as_u16(),
            body: format!("prompt optimizer upstream error: {detail}"),
        });
    }

    if let Some(value) = value.as_ref() {
        if let Some(text) = extract_text(value) {
            return Ok(text);
        }
    }
    // 部分供应商（如 Huawei CodeAgent 3）的 hook 会把请求强制改成流式，
    // 即使我们发的是 stream:false；这时响应是 SSE，按增量文本拼回去。
    if let Some(text) = extract_stream_text(&raw_text) {
        return Ok(text);
    }
    Err(AppError::Message(
        "prompt optimizer returned no text content".to_string(),
    ))
}

/// 从 SSE 响应里拼接增量文本。
pub(crate) fn extract_stream_text(raw: &str) -> Option<String> {
    let mut text = String::new();
    for line in raw.lines() {
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            continue;
        };

        // AI Provider Messages / OpenAI Responses
        if let Some(delta) = event.get("delta") {
            if let Some(chunk) = delta.get("text").and_then(Value::as_str) {
                text.push_str(chunk);
                continue;
            }
            if let Some(chunk) = delta.as_str() {
                text.push_str(chunk);
                continue;
            }
        }
        // OpenAI Chat Completions
        if let Some(chunk) = event
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("content"))
            .and_then(Value::as_str)
        {
            text.push_str(chunk);
            continue;
        }
        // Gemini Native
        if let Some(chunk) = event
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
            .and_then(|candidate| candidate.get("content"))
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
            .and_then(|parts| parts.first())
            .and_then(|part| part.get("text"))
            .and_then(Value::as_str)
        {
            text.push_str(chunk);
        }
    }
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(config: Value) -> Provider {
        Provider {
            id: "test".into(),
            name: "test".into(),
            settings_config: config,
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        }
    }

    #[test]
    fn extracts_text_from_all_supported_shapes() {
        let cases = [
            json!({"content": [{"type": "text", "text": "hello"}]}),
            json!({"choices": [{"message": {"content": "hello"}}]}),
            json!({"output": [{"content": [{"type": "output_text", "text": "hello"}]}]}),
            json!({"candidates": [{"content": {"parts": [{"text": "hello"}]}}]}),
        ];
        for body in cases {
            assert_eq!(extract_text(&body).as_deref(), Some("hello"), "{body}");
        }
    }

    #[test]
    fn joins_text_blocks_and_ignores_blank_ones() {
        let body = json!({"content": [
            {"type": "text", "text": " first "},
            {"type": "thinking", "thinking": "ignored"},
            {"type": "text", "text": "  "},
            {"type": "text", "text": "second"}
        ]});
        assert_eq!(extract_text(&body).as_deref(), Some("first\nsecond"));
    }

    #[test]
    fn returns_none_without_text() {
        assert_eq!(extract_text(&json!({"content": []})), None);
        assert_eq!(extract_text(&json!({"choices": []})), None);
        assert_eq!(extract_text(&json!({})), None);
    }

    #[test]
    fn builds_non_streaming_request_without_tools() {
        let body = build_request("m", "  keep me  ");
        assert_eq!(body["model"], "m");
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["content"], "  keep me  ");
        assert_eq!(body["tool_choice"], Value::Null);
        assert!(body["system"].as_str().unwrap().contains("提示词优化器"));
    }

    #[test]
    fn joins_streamed_deltas_from_each_protocol() {
        let anthropic = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hello \"}}\n\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\ndata: [DONE]\n";
        assert_eq!(
            extract_stream_text(anthropic).as_deref(),
            Some("hello world")
        );

        let chat = "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n";
        assert_eq!(extract_stream_text(chat).as_deref(), Some("hello"));

        let responses = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n";
        assert_eq!(extract_stream_text(responses).as_deref(), Some("hello"));

        let gemini = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello\"}]}}]}\n";
        assert_eq!(extract_stream_text(gemini).as_deref(), Some("hello"));
        assert_eq!(extract_stream_text("data: [DONE]\n"), None);
    }

    #[test]
    fn default_model_follows_app_config_shape() {
        let claude = provider(json!({"env": {"ANTHROPIC_MODEL": " claude-x "}}));
        assert_eq!(
            default_model(&AppType::Claude, &claude).as_deref(),
            Some("claude-x")
        );

        let codex = provider(json!({"model": "gpt-x"}));
        assert_eq!(
            default_model(&AppType::Codex, &codex).as_deref(),
            Some("gpt-x")
        );

        let gemini = provider(json!({"env": {"GEMINI_MODEL": "gemini-x"}}));
        assert_eq!(
            default_model(&AppType::Gemini, &gemini).as_deref(),
            Some("gemini-x")
        );

        assert_eq!(default_model(&AppType::Codex, &provider(json!({}))), None);
    }
}
