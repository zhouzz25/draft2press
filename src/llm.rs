use crate::config::ModelConfig;
use crate::cost::TokenUsage;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }
    #[allow(dead_code)]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: content.into() }
    }
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f64,
    max_tokens: usize,
    stream: bool,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: TokenUsage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    #[allow(dead_code)]
    role: String,
    content: String,
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
    #[allow(dead_code)]
    finish_reason: String,
}

pub struct LlmResponse {
    pub content: String,
    pub usage: TokenUsage,
}

// Per-tick streaming counters so the UI can show live progress even while a
// reasoning model is still thinking (and only reasoning_content is arriving).
#[derive(Clone, Copy, Default)]
pub struct StreamProgress {
    pub content: usize,
    pub reasoning: usize,
}

// Streaming failure modes: endpoint rejected stream_options (retryable without it),
// or a terminal error (partial = chars received before failure; >0 disables fallback).
#[derive(Debug)]
enum StreamFail {
    OptionsRejected,
    Terminal(anyhow::Error, usize),
}

pub struct LlmClient {
    config: ModelConfig,
    http: reqwest::Client,
    http_stream: reqwest::Client,
}

const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

impl LlmClient {
    pub fn new(config: ModelConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("Failed to build HTTP client");
        // Streaming requests: no overall deadline; each chunk read enforces its own idle timeout
        let http_stream = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("Failed to build streaming HTTP client");
        Self { config, http, http_stream }
    }

    // Streaming chat: reports cumulative generated char count via on_progress.
    // Falls back to a plain non-stream call when the endpoint rejects streaming.
    pub async fn chat_stream(&self, messages: &[Message], on_progress: impl Fn(StreamProgress) + Send + 'static) -> Result<LlmResponse> {
        if self.config.api_key.is_empty() {
            return Err(anyhow!("未配置 API Key，请在网页右上角「设置」中填写 API Key 后重试"));
        }
        match self.try_stream(messages, true, &on_progress).await {
            Ok(r) => Ok(r),
            Err(StreamFail::OptionsRejected) => {
                eprintln!("[llm] 端点拒绝 stream_options，去掉该字段重试");
                match self.try_stream(messages, false, &on_progress).await {
                    Ok(r) => Ok(r),
                    Err(_) => self.chat_with_max_tokens(messages, self.config.max_tokens).await,
                }
            }
            Err(StreamFail::Terminal(e, partial)) if partial > 0 => {
                eprintln!("[llm] 流式中断且已收到 {partial} 字，不重试以免重复计费: {e}");
                Err(e)
            }
            Err(StreamFail::Terminal(e, _)) => {
                eprintln!("[llm] 流式失败({e})，回退非流式调用");
                self.chat_with_max_tokens(messages, self.config.max_tokens).await
            }
        }
    }

    async fn try_stream(
        &self,
        messages: &[Message],
        include_stream_options: bool,
        on_progress: &impl Fn(StreamProgress),
    ) -> std::result::Result<LlmResponse, StreamFail> {
        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "temperature": self.config.temperature,
            "max_tokens": self.config.max_tokens,
            "stream": true,
        });
        if include_stream_options {
            body["stream_options"] = serde_json::json!({ "include_usage": true });
        }

        let resp = self.http_stream
            .post(&self.config.endpoint)
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .send()
            .await;

        let resp = resp.map_err(|e| {
            let cause = if e.is_connect() { "连接失败（网络/DNS问题）" } else { "未知网络错误" };
            StreamFail::Terminal(anyhow!("LLM API 请求失败（{cause}）: {e}"), 0)
        })?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if include_stream_options && (status == 400 || status == 422) {
                return Err(StreamFail::OptionsRejected);
            }
            return Err(StreamFail::Terminal(
                anyhow!("LLM API 返回错误 {}: {}", status, &text[..text.len().min(500)]),
                0,
            ));
        }

        use tokio_stream::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut usage = TokenUsage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 };
        let mut got_usage = false;
        let report = |content: &str, reasoning: &str| {
            on_progress(StreamProgress { content: content.chars().count(), reasoning: reasoning.chars().count() });
        };

        loop {
            let chunk = tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next()).await
                .map_err(|_| StreamFail::Terminal(anyhow!("流式响应读空闲超时（{STREAM_IDLE_TIMEOUT:?} 无数据）"), content.chars().count()))?
                .transpose()
                .map_err(|e| StreamFail::Terminal(anyhow!("读取流式响应失败: {e}"), content.chars().count()))?;
            let Some(bytes) = chunk else { break };
            buf.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim_end_matches('\r').to_string();
                buf.drain(..=pos);
                let Some(data) = line.strip_prefix("data:") else { continue };
                let data = data.trim();
                if data == "[DONE]" {
                    return Ok(LlmResponse { content, usage: if got_usage { usage } else { TokenUsage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 } } });
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else { continue };
                if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
                    usage = TokenUsage {
                        prompt_tokens: u.get("prompt_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as usize,
                        completion_tokens: u.get("completion_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as usize,
                        total_tokens: u.get("total_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as usize,
                    };
                    got_usage = true;
                }
                let delta = &v["choices"][0]["delta"];
                let mut changed = false;
                if let Some(c) = delta.get("content").and_then(|c| c.as_str()) {
                    content.push_str(c);
                    changed = true;
                }
                if let Some(rc) = delta.get("reasoning_content").and_then(|c| c.as_str()) {
                    reasoning.push_str(rc);
                    changed = true;
                }
                if changed { report(&content, &reasoning); }
            }
        }

        Ok(LlmResponse { content, usage: if got_usage { usage } else { TokenUsage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 } } })
    }

    pub async fn chat_with_max_tokens(&self, messages: &[Message], max_tokens: usize) -> Result<LlmResponse> {
        if self.config.api_key.is_empty() {
            return Err(anyhow!("未配置 API Key，请在网页右上角「设置」中填写 API Key 后重试"));
        }

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: messages.to_vec(),
            temperature: self.config.temperature,
            max_tokens,
            stream: false,
        };

        let resp = self.http
            .post(&self.config.endpoint)
            .bearer_auth(&self.config.api_key)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                let cause = if e.is_timeout() { "超时" }
                    else if e.is_connect() { "连接失败（网络/DNS问题）" }
                    else if e.is_request() { "请求构造错误" }
                    else { "未知网络错误" };
                eprintln!("[llm] 请求失败: {cause}, endpoint={}, model={}, error={e}", self.config.endpoint, self.config.model);
                anyhow!("LLM API 请求失败（{cause}）: {e}")
            })?;

        let status = resp.status();
        let body = resp.text().await.context("读取响应体失败（可能是 API 响应超时，max_tokens 过大或网络问题）")?;

        if !status.is_success() {
            return Err(anyhow!("LLM API 返回错误 {}: {}", status, body));
        }

        let chat_resp: ChatResponse =
            serde_json::from_str(&body).with_context(|| format!("解析响应 JSON 失败，原始内容: {body}"))?;

        let choice = chat_resp.choices.into_iter().next()
            .ok_or_else(|| anyhow!("响应中没有 choices"))?;

        let content = if !choice.message.content.is_empty() {
            choice.message.content
        } else if let Some(rc) = choice.message.reasoning_content {
            eprintln!("[llm] content 为空，使用 reasoning_content（长度 {}）", rc.len());
            rc
        } else {
            String::new()
        };

        Ok(LlmResponse { content, usage: chat_resp.usage })
    }
}
