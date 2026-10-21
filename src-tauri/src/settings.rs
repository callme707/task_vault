use std::fs;
use std::net::IpAddr;
use std::path::Path;
use std::str::FromStr;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::storage;

/// AI 提供商协议。OpenAI 兼容协议（DeepSeek/Kimi/Qwen/GLM/本地 Ollama 等）
/// 走 openai；Anthropic 官方 API 走 anthropic。
pub const PROVIDER_OPENAI: &str = "openai";
pub const PROVIDER_ANTHROPIC: &str = "anthropic";

pub const DEFAULT_MODEL: &str = "deepseek-v4-flash";
pub const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
pub const DEFAULT_QUICK_HOTKEY: &str = "Ctrl+Alt+Q";
pub const DEFAULT_MAIN_HOTKEY: &str = "Ctrl+Alt+W";
pub const REDACTED_API_KEY: &str = "********";
pub const PERSONA_AUTO: &str = "auto";
pub const PERSONA_DEV: &str = "dev";
pub const PERSONA_PM: &str = "pm";
pub const PERSONA_GTD: &str = "gtd";

pub fn normalize_persona(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        PERSONA_DEV => PERSONA_DEV,
        PERSONA_PM => PERSONA_PM,
        PERSONA_GTD => PERSONA_GTD,
        _ => PERSONA_AUTO,
    }
}

/// In-memory settings.  `api_key` is intentionally skipped by serde so an
/// accidental serialization of this runtime object cannot expose the secret.
/// Disk persistence is handled by [`Settings::save`] and uses a DPAPI envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(skip)]
    pub api_key: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
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
    #[serde(default = "default_true")]
    pub auto_parse: bool,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "default_quick_hotkey")]
    pub quick_hotkey: String,
    #[serde(default = "default_main_hotkey")]
    pub main_hotkey: String,
    #[serde(default = "default_persona")]
    pub default_persona: String,
}

fn default_provider() -> String {
    PROVIDER_OPENAI.to_string()
}

fn default_model() -> String {
    DEFAULT_MODEL.to_string()
}

fn default_base_url() -> String {
    DEFAULT_BASE_URL.to_string()
}

fn default_true() -> bool {
    true
}

fn default_quick_hotkey() -> String {
    DEFAULT_QUICK_HOTKEY.to_string()
}

fn default_persona() -> String {
    PERSONA_AUTO.to_string()
}

fn default_main_hotkey() -> String {
    DEFAULT_MAIN_HOTKEY.to_string()
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
            default_persona: PERSONA_AUTO.to_string(),
        }
    }
}

/// Settings returned to the UI.  Only key presence crosses the command
/// boundary; the runtime key never does.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsView {
    pub has_api_key: bool,
    pub base_url: String,
    pub model: String,
    /// openai / anthropic
    pub provider: String,
    pub vision_model: Option<String>,
    pub disable_reasoning: bool,
    pub auto_parse: bool,
    pub autostart: bool,
    pub quick_hotkey: String,
    pub main_hotkey: String,
    #[serde(default = "default_persona")]
    pub default_persona: String,
}

/// Settings received from the UI.  The key is optional so an omitted or empty
/// value retains the saved key.  `remove_api_key` is the explicit clear action.
#[derive(Debug, Clone, Deserialize)]
pub struct SettingsUpdate {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub remove_api_key: bool,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default)]
    pub vision_model: Option<String>,
    #[serde(default = "default_true")]
    pub disable_reasoning: bool,
    #[serde(default = "default_true")]
    pub auto_parse: bool,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "default_quick_hotkey")]
    pub quick_hotkey: String,
    #[serde(default = "default_main_hotkey")]
    pub main_hotkey: String,
    #[serde(default = "default_persona")]
    pub default_persona: String,
}

impl Settings {
    pub fn provider(&self) -> &str {
        if self.provider.eq_ignore_ascii_case(PROVIDER_ANTHROPIC) {
            PROVIDER_ANTHROPIC
        } else {
            PROVIDER_OPENAI
        }
    }

    /// 图片解析走 vision_model（非空时），否则回落主模型
    pub fn model_for(&self, with_image: bool) -> &str {
        match self
            .vision_model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(m) if with_image => m,
            _ => self.model.as_str(),
        }
    }

    /// Anthropic 协议的接口地址不带 /v1（SDK 路径为 /v1/messages），
    /// 用户填官方地址 https://api.anthropic.com 即可；OpenAI 兼容地址保持原样。
    pub fn normalized_base_url(&self) -> String {
        let mut url = self.base_url.trim().trim_end_matches('/').to_string();
        if self.provider() == PROVIDER_ANTHROPIC && url.ends_with("/v1") {
            url.truncate(url.len() - 3);
        }
        url
    }

    /// Validate the provider, endpoint and model before a setting can be
    /// persisted or used for a request.
    pub fn validate(&self) -> Result<(), String> {
        match self.provider.trim().to_ascii_lowercase().as_str() {
            PROVIDER_OPENAI | PROVIDER_ANTHROPIC => {}
            other => return Err(format!("不支持的 AI 协议: {other}")),
        }
        self.validate_base_url()?;
        if self.model.trim().is_empty() {
            return Err("模型名称不能为空".to_string());
        }
        Ok(())
    }

    /// Validate the endpoint independently when a caller wants to show a
    /// precise URL error in the settings UI.
    pub fn validate_base_url(&self) -> Result<(), String> {
        let raw = self.base_url.trim();
        let url = parse_base_url(raw)?;
        if url.query().is_some() || url.fragment().is_some() {
            return Err("服务地址不能包含查询参数或片段".to_string());
        }

        // `Url::username` does not distinguish an empty username in `@host`,
        // so inspect the authority as well as the parsed accessors.
        let authority = raw
            .split_once("://")
            .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(rest))
            .unwrap_or_default();
        if authority.contains('@') || !url.username().is_empty() || url.password().is_some() {
            return Err("服务地址不能包含用户名或密码".to_string());
        }

        match url.scheme() {
            "https" => Ok(()),
            "http" if is_loopback_host(url.host_str()) => Ok(()),
            "http" => Err("仅允许 HTTPS；HTTP 只支持本机回环地址".to_string()),
            scheme => Err(format!("不支持的服务地址协议: {scheme}")),
        }
    }

    /// Whether the configured provider can be called.  A configured key is
    /// required for network endpoints; OpenAI-compatible loopback endpoints
    /// (for example local Ollama) are allowed without a key.
    pub fn api_ready(&self) -> bool {
        if self.validate().is_err() {
            return false;
        }
        if self.effective_api_key().is_some() {
            return true;
        }
        self.provider.trim().eq_ignore_ascii_case(PROVIDER_OPENAI)
            && self
                .validate_base_url()
                .is_ok_and(|_| is_loopback_base_url(self.base_url.trim()))
    }

    pub fn view(&self) -> SettingsView {
        let has_api_key = !self.api_key.trim().is_empty();
        SettingsView {
            has_api_key,
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            provider: self.provider.clone(),
            vision_model: self.vision_model.clone(),
            disable_reasoning: self.disable_reasoning,
            auto_parse: self.auto_parse,
            autostart: self.autostart,
            quick_hotkey: self.quick_hotkey.clone(),
            main_hotkey: self.main_hotkey.clone(),
            default_persona: normalize_persona(&self.default_persona).to_string(),
        }
    }

    /// Load settings or create a valid default file.  Existing malformed JSON,
    /// invalid ciphertext and invalid endpoints are returned as errors without
    /// changing the original file.
    pub fn load_or_create(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            let settings = Settings::default();
            settings.save(path)?;
            return Ok(settings);
        }

        let text = fs::read_to_string(path).map_err(|e| format!("读取设置失败: {e}"))?;
        let persisted: PersistedSettings =
            serde_json::from_str(&text).map_err(|e| format!("设置文件格式无效: {e}"))?;
        let (settings, needs_migration) = persisted.into_settings()?;
        settings.validate()?;

        if needs_migration {
            // `save` uses an atomic replacement, so a failed migration leaves
            // the legacy file intact and never creates a plaintext backup.
            settings.save(path)?;
        }
        Ok(settings)
    }

    /// Persist non-secret settings in readable JSON and the API key as a
    /// base64-encoded current-user DPAPI envelope.  Memory is not changed by
    /// this method; callers should update their state only after it succeeds.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let persisted = PersistedSettings::from_settings(self)?;
        let text =
            serde_json::to_vec_pretty(&persisted).map_err(|e| format!("序列化设置失败: {e}"))?;
        storage::atomic_write(path, &text)
    }

    /// Apply a complete non-secret settings update while retaining an omitted
    /// or empty API key.  The returned value is validated but not persisted.
    pub fn apply_update(&self, update: &SettingsUpdate) -> Result<Self, String> {
        update.merge(self)
    }

    /// API Key 优先取配置。只有标准服务 origin 才读取对应环境变量：
    /// api.deepseek.com 使用 DEEPSEEK_API_KEY，api.openai.com 使用
    /// OPENAI_API_KEY，api.anthropic.com 使用 ANTHROPIC_API_KEY。自定义网关
    /// 不继承全局环境密钥；本机 OpenAI 兼容回环服务可以无 key 运行。
    pub fn effective_api_key(&self) -> Option<String> {
        let trimmed = self.api_key.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
        let env_name = environment_api_key_name(self)?;
        std::env::var(env_name)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }
}

impl SettingsUpdate {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            api_key: None,
            remove_api_key: false,
            base_url: settings.base_url.clone(),
            model: settings.model.clone(),
            provider: settings.provider.clone(),
            vision_model: settings.vision_model.clone(),
            disable_reasoning: settings.disable_reasoning,
            auto_parse: settings.auto_parse,
            autostart: settings.autostart,
            quick_hotkey: settings.quick_hotkey.clone(),
            main_hotkey: settings.main_hotkey.clone(),
            default_persona: normalize_persona(&settings.default_persona).to_string(),
        }
    }

    pub fn merge(&self, current: &Settings) -> Result<Settings, String> {
        let mut next = current.clone();
        let supplied_api_key = self
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty() && *key != REDACTED_API_KEY);
        if self.remove_api_key {
            next.api_key.clear();
        } else if let Some(api_key) = supplied_api_key {
            next.api_key = api_key.to_string();
        }

        next.base_url = self.base_url.trim().to_string();
        next.model = self.model.trim().to_string();
        next.provider = self.provider.trim().to_ascii_lowercase();
        next.vision_model = self
            .vision_model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(ToOwned::to_owned);
        next.disable_reasoning = self.disable_reasoning;
        next.auto_parse = self.auto_parse;
        next.autostart = self.autostart;
        next.quick_hotkey = self.quick_hotkey.clone();
        next.main_hotkey = self.main_hotkey.clone();
        next.default_persona = normalize_persona(&self.default_persona).to_string();
        next.validate()?;

        // A saved key belongs to the provider and service origin it was
        // entered for.  Reusing it after changing either is surprising and
        // can send a credential to the wrong service.  A path-only change on
        // the same origin remains compatible.
        let origin_changed = match (
            endpoint_origin(&current.base_url),
            endpoint_origin(&next.base_url),
        ) {
            (Ok(old), Ok(new)) => old != new,
            _ => true,
        };
        let provider_changed = !current.provider.eq_ignore_ascii_case(&next.provider);
        if !current.api_key.trim().is_empty()
            && !self.remove_api_key
            && supplied_api_key.is_none()
            && (provider_changed || origin_changed)
        {
            return Err("服务地址已变化，请重新填写密钥或清除已保存密钥".to_string());
        }
        Ok(next)
    }
}

fn parse_base_url(raw: &str) -> Result<Url, String> {
    if raw.is_empty() {
        return Err("服务地址不能为空".to_string());
    }
    let url = Url::parse(raw).map_err(|e| format!("服务地址无效: {e}"))?;
    if url.host_str().is_none() {
        return Err("服务地址必须包含主机名".to_string());
    }
    Ok(url)
}

fn is_loopback_base_url(raw: &str) -> bool {
    Url::parse(raw)
        .ok()
        .is_some_and(|url| url.scheme() == "http" && is_loopback_host(url.host_str()))
}

fn is_loopback_host(host: Option<&str>) -> bool {
    let Some(host) = host else {
        return false;
    };
    let host = host
        .trim_end_matches('.')
        .trim_matches(|character| character == '[' || character == ']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    IpAddr::from_str(host).is_ok_and(|address| address.is_loopback())
}

#[derive(Debug, PartialEq, Eq)]
struct EndpointOrigin {
    scheme: String,
    host: String,
    port: Option<u16>,
}

fn endpoint_origin(raw: &str) -> Result<EndpointOrigin, String> {
    let url = parse_base_url(raw)?;
    Ok(EndpointOrigin {
        scheme: url.scheme().to_ascii_lowercase(),
        host: url
            .host_str()
            .unwrap_or_default()
            .trim_end_matches('.')
            .trim_matches(|character| character == '[' || character == ']')
            .to_ascii_lowercase(),
        port: url.port_or_known_default(),
    })
}

/// Map only the standard provider origins to their matching environment key.
/// The mapping is intentionally pure so callers can validate it without
/// reading or mutating process-global environment state.
fn environment_api_key_name(settings: &Settings) -> Option<&'static str> {
    let origin = endpoint_origin(settings.base_url.trim()).ok()?;
    let provider = settings.provider.trim().to_ascii_lowercase();
    match (
        provider.as_str(),
        origin.scheme.as_str(),
        origin.host.as_str(),
        origin.port,
    ) {
        (PROVIDER_OPENAI, "https", "api.deepseek.com", Some(443)) => Some("DEEPSEEK_API_KEY"),
        (PROVIDER_OPENAI, "https", "api.openai.com", Some(443)) => Some("OPENAI_API_KEY"),
        (PROVIDER_ANTHROPIC, "https", "api.anthropic.com", Some(443)) => Some("ANTHROPIC_API_KEY"),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key_protected: Option<String>,
    /// Read only for migration from versions that wrote plaintext `api_key`.
    /// It is never emitted by serialization.
    #[serde(default, skip_serializing)]
    api_key: Option<String>,
    #[serde(default = "default_base_url")]
    base_url: String,
    #[serde(default = "default_model")]
    model: String,
    #[serde(default = "default_provider")]
    provider: String,
    #[serde(default)]
    vision_model: Option<String>,
    #[serde(default = "default_true")]
    disable_reasoning: bool,
    #[serde(default = "default_true")]
    auto_parse: bool,
    #[serde(default)]
    autostart: bool,
    #[serde(default = "default_quick_hotkey")]
    quick_hotkey: String,
    #[serde(default = "default_main_hotkey")]
    main_hotkey: String,
    #[serde(default = "default_persona")]
    default_persona: String,
}

impl PersistedSettings {
    fn from_settings(settings: &Settings) -> Result<Self, String> {
        let api_key_protected = if settings.api_key.trim().is_empty() {
            None
        } else {
            let protected = storage::protect(settings.api_key.trim().as_bytes())?;
            Some(B64.encode(protected))
        };
        Ok(Self {
            api_key_protected,
            api_key: None,
            base_url: settings.base_url.clone(),
            model: settings.model.clone(),
            provider: settings.provider.clone(),
            vision_model: settings.vision_model.clone(),
            disable_reasoning: settings.disable_reasoning,
            auto_parse: settings.auto_parse,
            autostart: settings.autostart,
            quick_hotkey: settings.quick_hotkey.clone(),
            main_hotkey: settings.main_hotkey.clone(),
            default_persona: normalize_persona(&settings.default_persona).to_string(),
        })
    }

    fn into_settings(self) -> Result<(Settings, bool), String> {
        let has_legacy_key = self.api_key.is_some();
        let api_key = match self.api_key_protected {
            Some(encoded) => {
                let encoded = encoded.trim();
                if encoded.is_empty() {
                    return Err("受保护的 API Key 为空".to_string());
                }
                let protected = B64
                    .decode(encoded)
                    .map_err(|e| format!("受保护的 API Key 编码无效: {e}"))?;
                let plaintext = storage::unprotect(&protected)?;
                String::from_utf8(plaintext)
                    .map_err(|_| "受保护的 API Key 不是有效 UTF-8".to_string())?
            }
            None => self.api_key.unwrap_or_default(),
        };
        Ok((
            Settings {
                api_key,
                base_url: self.base_url,
                model: self.model,
                provider: self.provider,
                vision_model: self.vision_model,
                disable_reasoning: self.disable_reasoning,
                auto_parse: self.auto_parse,
                autostart: self.autostart,
                quick_hotkey: self.quick_hotkey,
                main_hotkey: self.main_hotkey,
                default_persona: normalize_persona(&self.default_persona).to_string(),
            },
            has_legacy_key,
        ))
    }
}

#[cfg(test)]
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "taskvault-settings-test-{}-{}-{}",
                std::process::id(),
                label,
                id
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let Ok(temp_root) = fs::canonicalize(std::env::temp_dir()) else {
                return;
            };
            let Ok(candidate) = fs::canonicalize(&self.0) else {
                return;
            };
            let safe_name = candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("taskvault-settings-test-"));
            if candidate.starts_with(temp_root) && safe_name {
                let _ = fs::remove_dir_all(candidate);
            }
        }
    }

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

    #[test]
    fn endpoint_validation_allows_https_and_loopback_http_only() {
        let mut settings = Settings::default();
        for endpoint in [
            "https://api.example.com",
            "https://api.example.com/v1/",
            "http://localhost:11434/v1",
            "http://127.0.0.1:11434/v1",
            "http://[::1]:11434/v1",
        ] {
            settings.base_url = endpoint.to_string();
            assert!(settings.validate_base_url().is_ok(), "{endpoint}");
        }

        for endpoint in [
            "http://api.example.com",
            "https://user:password@api.example.com",
            "https://api.example.com?token=secret",
            "ftp://api.example.com",
        ] {
            settings.base_url = endpoint.to_string();
            assert!(settings.validate_base_url().is_err(), "{endpoint}");
        }
    }

    #[test]
    fn environment_key_names_are_bound_to_standard_origins() {
        let cases = [
            (
                PROVIDER_OPENAI,
                "https://api.deepseek.com/v1",
                Some("DEEPSEEK_API_KEY"),
            ),
            (
                PROVIDER_OPENAI,
                "https://api.openai.com/v1",
                Some("OPENAI_API_KEY"),
            ),
            (
                PROVIDER_ANTHROPIC,
                "https://api.anthropic.com/v1",
                Some("ANTHROPIC_API_KEY"),
            ),
            (PROVIDER_OPENAI, "https://gateway.example.com/v1", None),
            (PROVIDER_OPENAI, "http://localhost:11434/v1", None),
            (PROVIDER_ANTHROPIC, "https://api.deepseek.com/v1", None),
            (PROVIDER_OPENAI, "https://api.deepseek.com:8443/v1", None),
        ];

        for (provider, base_url, expected) in cases {
            let settings = Settings {
                provider: provider.to_string(),
                base_url: base_url.to_string(),
                ..Settings::default()
            };
            assert_eq!(environment_api_key_name(&settings), expected, "{base_url}");
        }
    }

    #[test]
    fn view_redacts_key_and_update_merges_key_safely() {
        let current = Settings {
            api_key: "sk-secret-value".to_string(),
            ..Settings::default()
        };
        let view = current.view();
        let json = serde_json::to_string(&view).expect("serialize view");
        assert!(view.has_api_key);
        assert!(!json.contains("sk-secret-value"));

        let mut update = SettingsUpdate::from_settings(&current);
        update.api_key = Some(String::new());
        let retained = update.merge(&current).expect("merge retained key");
        assert_eq!(retained.api_key, "sk-secret-value");

        update.remove_api_key = true;
        let removed = update.merge(&current).expect("remove key");
        assert!(removed.api_key.is_empty());

        let mut path_only = SettingsUpdate::from_settings(&current);
        path_only.base_url = "https://api.deepseek.com/v1".to_string();
        assert!(path_only.merge(&current).is_ok());

        let mut changed_origin = SettingsUpdate::from_settings(&current);
        changed_origin.base_url = "https://api.example.com".to_string();
        let error = changed_origin.merge(&current).unwrap_err();
        assert!(error.contains("服务地址已变化"));
    }

    #[test]
    fn corrupt_settings_are_rejected_without_overwrite() {
        let dir = TestDir::new("corrupt");
        let path = dir.path().join("settings.json");
        let original = br#"{"base_url": "https://api.example.com", "broken""#;
        fs::write(&path, original).expect("seed corrupt settings");

        assert!(Settings::load_or_create(&path).is_err());
        assert_eq!(fs::read(&path).expect("read original"), original);
    }

    #[test]
    fn invalid_endpoint_does_not_replace_existing_file() {
        let dir = TestDir::new("invalid-url");
        let path = dir.path().join("settings.json");
        Settings::default().save(&path).expect("seed settings");
        let original = fs::read(&path).expect("read original");

        let invalid = Settings {
            base_url: "http://public.example.com".to_string(),
            ..Settings::default()
        };
        assert!(invalid.save(&path).is_err());
        assert_eq!(fs::read(&path).expect("read preserved settings"), original);
    }

    #[cfg(windows)]
    #[test]
    fn protected_key_roundtrip_and_legacy_migration() {
        let dir = TestDir::new("migration");
        let path = dir.path().join("settings.json");
        let settings = Settings {
            api_key: "legacy-secret-value".to_string(),
            ..Settings::default()
        };
        settings.save(&path).expect("save protected settings");
        let protected_text = fs::read_to_string(&path).expect("read protected settings");
        assert!(!protected_text.contains("legacy-secret-value"));
        assert!(protected_text.contains("api_key_protected"));
        assert_eq!(
            Settings::load_or_create(&path).unwrap().api_key,
            settings.api_key
        );

        let legacy = r#"{"api_key":"legacy-plaintext","base_url":"https://api.example.com","model":"model","auto_parse":true,"autostart":false,"quick_hotkey":"Ctrl+Alt+Q","main_hotkey":"Ctrl+Alt+W"}"#;
        fs::write(&path, legacy).expect("seed legacy settings");
        let loaded = Settings::load_or_create(&path).expect("migrate legacy settings");
        assert_eq!(loaded.api_key, "legacy-plaintext");
        let migrated_text = fs::read_to_string(&path).expect("read migrated settings");
        assert!(!migrated_text.contains("legacy-plaintext"));
        assert!(migrated_text.contains("api_key_protected"));
        assert!(!path.with_extension("json.bak").exists());
    }

    #[cfg(not(windows))]
    #[test]
    fn legacy_key_migration_fails_closed_without_overwrite_off_windows() {
        let dir = TestDir::new("migration-off-windows");
        let path = dir.path().join("settings.json");
        let legacy = br#"{"api_key":"legacy-plaintext","base_url":"https://api.example.com","model":"model","auto_parse":true,"autostart":false,"quick_hotkey":"Ctrl+Alt+Q","main_hotkey":"Ctrl+Alt+W"}"#;
        fs::write(&path, legacy).expect("seed legacy settings");
        assert!(Settings::load_or_create(&path).is_err());
        assert_eq!(fs::read(&path).expect("read legacy settings"), legacy);
    }
}
