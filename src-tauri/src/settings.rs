use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// AI 提供商协议。OpenAI 兼容协议（DeepSeek/Kimi/Qwen/GLM/本地 Ollama 等）
/// 走 openai；Anthropic 官方 API 走 anthropic。
pub const PROVIDER_OPENAI: &str = "openai";
pub const PROVIDER_ANTHROPIC: &str = "anthropic";

pub const DEFAULT_MODEL: &str = "deepseek-v4-flash";
pub const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
pub const DEFAULT_QUICK_HOTKEY: &str = "Ctrl+Alt+Q";
pub const DEFAULT_MAIN_HOTKEY: &str = "Ctrl+Alt+W";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    /// openai / anthropic
    #[serde(default = "default_provider")]
    pub provider: String,
    /// 解析图片用的视觉模型；留空则用主模型（主模型需自身支持视觉）
    #[serde(default)]
    pub vision_model: Option<String>,
    /// 图片解析时尝试关闭模型推理/思考（推理 token 无法不计入输出上限，关掉才是根治）。
    /// 会同时携带 DeepSeek/GLM、Qwen、vLLM 三种常见参数写法，服务商不认识的会忽略；
    /// 个别严格校验的服务商若因此报参数错误，可在设置中关闭。
    #[serde(default = "default_true")]
    pub disable_reasoning: bool,
    pub auto_parse: bool,
    pub autostart: bool,
    pub quick_hotkey: String,
    pub main_hotkey: String,
}

fn default_provider() -> String {
    PROVIDER_OPENAI.to_string()
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            provider: PROVIDER_OPENAI.to_string(),
            vision_model: None,
            disable_reasoning: true,
            auto_parse: true,
            autostart: false,
            quick_hotkey: DEFAULT_QUICK_HOTKEY.to_string(),
            main_hotkey: DEFAULT_MAIN_HOTKEY.to_string(),
        }
    }
}

impl Settings {
    pub fn provider(&self) -> &str {
        if self.provider == PROVIDER_ANTHROPIC {
            PROVIDER_ANTHROPIC
        } else {
            PROVIDER_OPENAI
        }
    }

    /// 图片解析走 vision_model（非空时），否则回落主模型
    pub fn model_for(&self, with_image: bool) -> &str {
        match self.vision_model.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(m) if with_image => m,
            _ => self.model.as_str(),
        }
    }

    /// Anthropic 协议的接口地址不带 /v1（SDK 路径为 /v1/messages），
    /// 用户填官方地址 https://api.anthropic.com 即可；OpenAI 兼容地址保持原样。
    pub fn normalized_base_url(&self) -> String {
        let mut url = self.base_url.trim().trim_end_matches('/').to_string();
        if url.is_empty() {
            url = DEFAULT_BASE_URL.to_string();
        }
        if self.provider() == PROVIDER_ANTHROPIC && url.ends_with("/v1") {
            url.truncate(url.len() - 3);
        }
        url
    }

    pub fn load_or_create(path: &Path) -> Result<Self, String> {
        if path.exists() {
            let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
            let mut s: Settings = serde_json::from_str(&text).unwrap_or_default();
            if s.model.trim().is_empty() {
                s.model = DEFAULT_MODEL.to_string();
            }
            if s.base_url.trim().is_empty() {
                s.base_url = DEFAULT_BASE_URL.to_string();
            }
            Ok(s)
        } else {
            let s = Settings::default();
            s.save(path)?;
            Ok(s)
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, text).map_err(|e| e.to_string())
    }

    /// API Key 优先取配置，配置为空时允许使用环境变量。
    /// openai 协议读 DEEPSEEK_API_KEY 或 OPENAI_API_KEY；anthropic 读 ANTHROPIC_API_KEY。
    pub fn effective_api_key(&self) -> Option<String> {
        let trimmed = self.api_key.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
        let env_names: &[&str] = match self.provider() {
            PROVIDER_ANTHROPIC => &["ANTHROPIC_API_KEY"],
            _ => &["DEEPSEEK_API_KEY", "OPENAI_API_KEY"],
        };
        env_names
            .iter()
            .find_map(|n| std::env::var(n).ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_defaults_and_normalization() {
        let mut s = Settings::default();
        assert_eq!(s.provider(), PROVIDER_OPENAI);

        // anthropic + 官方地址：去掉尾部 /v1，避免 /v1/v1
        s.provider = PROVIDER_ANTHROPIC.into();
        s.base_url = "https://api.anthropic.com/v1".into();
        assert_eq!(s.normalized_base_url(), "https://api.anthropic.com");

        // openai 兼容地址保持原样（多数网关以 /v1 结尾）
        s.provider = PROVIDER_OPENAI.into();
        s.base_url = "https://api.deepseek.com/v1".into();
        assert_eq!(s.normalized_base_url(), "https://api.deepseek.com/v1");

        // 旧配置文件没有 provider 字段时 serde 回退到 openai
        let mut legacy = serde_json::from_str::<Settings>(
            r#"{"api_key":"","base_url":"https://api.deepseek.com","model":"deepseek-v4-flash","auto_parse":true,"autostart":false,"quick_hotkey":"Ctrl+Alt+Q","main_hotkey":"Ctrl+Alt+W"}"#,
        )
        .unwrap();
        assert_eq!(legacy.provider(), PROVIDER_OPENAI);
        assert!(legacy.vision_model.is_none());

        // 视觉模型：留空/空白回落主模型，配置了则图片走视觉模型
        assert_eq!(legacy.model_for(false), "deepseek-v4-flash");
        assert_eq!(legacy.model_for(true), "deepseek-v4-flash");
        legacy.vision_model = Some("  ".into());
        assert_eq!(legacy.model_for(true), "deepseek-v4-flash");
        legacy.vision_model = Some("glm-4v-flash".into());
        assert_eq!(legacy.model_for(true), "glm-4v-flash");
        assert_eq!(legacy.model_for(false), "deepseek-v4-flash");
    }
}
