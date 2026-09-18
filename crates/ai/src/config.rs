//! AI 端点配置。
//!
//! 只要对方兼容 OpenAI 的 `/chat/completions` 规范就能接：
//! DeepSeek、OpenAI、Moonshot、智谱、以及本地的 Ollama / LM Studio。

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn default_base_url() -> String {
    "https://api.deepseek.com/v1".to_string()
}

fn default_model() -> String {
    "deepseek-chat".to_string()
}

fn default_temperature() -> f32 {
    0.7
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AiConfig {
    /// 形如 `https://api.deepseek.com/v1`，本地 Ollama 为 `http://localhost:11434/v1`
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// 行内续写用的模型，留空则复用 `model`。续写建议用更快更便宜的模型。
    #[serde(default)]
    pub completion_model: Option<String>,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            base_url: default_base_url(),
            api_key: String::new(),
            model: default_model(),
            temperature: default_temperature(),
            max_tokens: None,
            completion_model: None,
        }
    }
}

impl AiConfig {
    pub fn config_dir() -> PathBuf {
        // 优先 APPDATA / XDG，退化到当前目录，保证任何环境都能落盘
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("fastnote")
    }

    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.json")
    }

    /// 读取配置：文件优先，环境变量覆盖。
    ///
    /// 环境变量方便临时切换端点：`FASTNOTE_API_KEY` / `FASTNOTE_BASE_URL` / `FASTNOTE_MODEL`。
    /// 未配置 key 时也不报错——AI 功能降级关闭，编辑器照常使用。
    pub fn load() -> Self {
        let mut cfg = std::fs::read_to_string(Self::config_path())
            .ok()
            .and_then(|s| serde_json::from_str::<AiConfig>(&s).ok())
            .unwrap_or_default();

        if let Ok(v) = std::env::var("FASTNOTE_BASE_URL") {
            if !v.trim().is_empty() {
                cfg.base_url = v;
            }
        }
        if let Ok(v) = std::env::var("FASTNOTE_API_KEY") {
            if !v.trim().is_empty() {
                cfg.api_key = v;
            }
        }
        if let Ok(v) = std::env::var("FASTNOTE_MODEL") {
            if !v.trim().is_empty() {
                cfg.model = v;
            }
        }
        cfg
    }

    pub fn save(&self) -> Result<()> {
        let dir = Self::config_dir();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("创建配置目录失败: {}", dir.display()))?;
        let path = Self::config_path();
        std::fs::write(&path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("写入配置失败: {}", path.display()))?;
        Ok(())
    }

    /// 本地端点（Ollama / LM Studio）不需要 key，因此单独判断。
    pub fn is_configured(&self) -> bool {
        if self.base_url.trim().is_empty() || self.model.trim().is_empty() {
            return false;
        }
        !self.api_key.trim().is_empty() || self.is_local()
    }

    pub fn is_local(&self) -> bool {
        let u = self.base_url.to_ascii_lowercase();
        u.contains("localhost") || u.contains("127.0.0.1") || u.contains("0.0.0.0")
    }

    pub fn chat_endpoint(&self) -> String {
        format!(
            "{}/chat/completions",
            self.base_url.trim_end_matches('/')
        )
    }

    pub fn completion_model(&self) -> &str {
        self.completion_model.as_deref().unwrap_or(&self.model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_normalizes_trailing_slash() {
        let mut c = AiConfig::default();
        c.base_url = "https://x.com/v1/".into();
        assert_eq!(c.chat_endpoint(), "https://x.com/v1/chat/completions");
    }

    #[test]
    fn local_endpoint_needs_no_key() {
        let mut c = AiConfig::default();
        c.base_url = "http://localhost:11434/v1".into();
        c.model = "qwen2.5".into();
        assert!(c.is_local());
        assert!(c.is_configured());
    }

    #[test]
    fn remote_endpoint_without_key_is_unconfigured() {
        let c = AiConfig::default();
        assert!(!c.is_configured());
    }

    #[test]
    fn completion_model_falls_back_to_main_model() {
        let c = AiConfig::default();
        assert_eq!(c.completion_model(), c.model);
    }
}
