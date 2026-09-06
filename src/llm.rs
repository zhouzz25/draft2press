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

pub struct LlmClient {
    config: ModelConfig,
    http: reqwest::Client,
}

impl LlmClient {
    pub fn new(config: ModelConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("Failed to build HTTP client");
        Self { config, http }
    }

    pub async fn chat(&self, messages: &[Message]) -> Result<LlmResponse> {
        self.chat_with_max_tokens(messages, self.config.max_tokens).await
    }

    pub async fn chat_with_max_tokens(&self, messages: &[Message], max_tokens: usize) -> Result<LlmResponse> {
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
