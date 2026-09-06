use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pricing {
    #[serde(default)]
    pub input_per_1k: f64,
    #[serde(default)]
    pub output_per_1k: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_context_length")]
    pub context_length: usize,
    #[serde(default)]
    pub thinking_mode: bool,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    #[serde(default)]
    pub pricing: Pricing,
    #[serde(default)]
    pub vision_endpoint: Option<String>,
    #[serde(default)]
    pub vision_api_key: Option<String>,
    #[serde(default)]
    pub vision_model: Option<String>,
    #[serde(default)]
    pub vision_max_tokens: Option<usize>,
    #[serde(default)]
    pub vision_desc_max_chars: Option<usize>,
    #[serde(default)]
    pub wx_app_id: Option<String>,
    #[serde(default)]
    pub wx_app_secret: Option<String>,
    #[serde(default)]
    pub token_budget: Option<usize>,
}

fn default_endpoint() -> String {
    "https://api.openai.com/v1/chat/completions".into()
}

fn default_model() -> String {
    "gpt-4o-mini".into()
}

fn default_context_length() -> usize {
    128_000
}

fn default_temperature() -> f64 {
    0.7
}

fn default_max_tokens() -> usize {
    100000
}

impl ModelConfig {
    pub fn load(config_path: &str) -> Result<Self> {
        // Auto-create .env if not exists
        if !std::path::Path::new(".env").exists() {
            let _ = std::fs::write(".env", "# API_KEY=sk-your-key-here\n");
        }
        let _ = dotenvy::dotenv();

        let mut config = if std::path::Path::new(config_path).exists() {
            let content = std::fs::read_to_string(config_path)
                .with_context(|| format!("读取配置文件失败: {}", config_path))?;
            toml::from_str::<ModelConfig>(&content)
                .with_context(|| format!("解析配置文件失败: {}", config_path))?
        } else {
            ModelConfig::default()
        };

        if let Ok(key) = std::env::var("API_KEY")
            && !key.is_empty()
        {
            config.api_key = key;
        }

        if config.api_key.is_empty() {
            eprintln!("提示：未配置 API Key，请在网页设置页填写，或在 .env 中设置 API_KEY");
            // Don't error, let user configure via web UI
        }

        Ok(config)
    }

    fn default() -> Self {
        Self {
            endpoint: default_endpoint(),
            api_key: String::new(),
            model: default_model(),
            context_length: default_context_length(),
            thinking_mode: false,
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            pricing: Pricing::default(),
            vision_endpoint: None,
            vision_api_key: None,
            vision_model: None,
            vision_max_tokens: None,
            vision_desc_max_chars: None,
            wx_app_id: None,
            wx_app_secret: None,
            token_budget: None,
        }
    }
}
