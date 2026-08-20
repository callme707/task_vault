use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const DEFAULT_MODEL: &str = "deepseek-v4-flash";
pub const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
pub const DEFAULT_QUICK_HOTKEY: &str = "Ctrl+Alt+Q";
pub const DEFAULT_MAIN_HOTKEY: &str = "Ctrl+Alt+W";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub auto_parse: bool,
    pub autostart: bool,
    pub quick_hotkey: String,
    pub main_hotkey: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            auto_parse: true,
            autostart: false,
            quick_hotkey: DEFAULT_QUICK_HOTKEY.to_string(),
            main_hotkey: DEFAULT_MAIN_HOTKEY.to_string(),
        }
    }
}

impl Settings {
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

    /// API Key 优先取配置，配置为空时允许使用环境变量 DEEPSEEK_API_KEY。
    pub fn effective_api_key(&self) -> Option<String> {
        if !self.api_key.trim().is_empty() {
            return Some(self.api_key.trim().to_string());
        }
        std::env::var("DEEPSEEK_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
    }
}
