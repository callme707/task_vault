use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use chrono::Local;
use serde_json::{json, Value};

use crate::models::AiParsed;
use crate::settings::{Settings, PROVIDER_ANTHROPIC};

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("未配置 API Key")]
    NoApiKey,
    #[error("网络错误: {0}")]
    Network(#[from] reqwest::Error),
    #[error("API 返回异常: {0}")]
    Api(String),
    #[error("返回内容不是合法 JSON: {0}")]
    BadJson(#[from] serde_json::Error),
}

/// 随记录提交给视觉模型的一张图片。
#[derive(Debug, Clone)]
pub struct ImageInput {
    /// image/png / image/jpeg / image/webp / image/gif
    pub mime: String,
    /// 不含 data: 前缀的纯 base64
    pub data: String,
}

/// 单图大小上限（Anthropic 单请求 5MB，留余量）
pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

/// 按文件头魔数识别常见图片格式
pub fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

pub fn encode_image(bytes: &[u8]) -> Result<ImageInput, AiError> {
    let mime = sniff_image_mime(bytes)
        .ok_or_else(|| AiError::Api("不支持的图片格式（仅支持 PNG/JPEG/WebP/GIF）".into()))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(AiError::Api(format!(
            "图片过大（{} MB），请压缩或截图后重试（上限 4 MB）",
            bytes.len() / 1024 / 1024
        )));
    }
    Ok(ImageInput {
        mime: mime.to_string(),
        data: B64.encode(bytes),
    })
}

const SYSTEM_PROMPT: &str = r#"你是一个任务收件箱整理助手。用户会把一句话需求、待办、想法或聊天记录发给你。
请只输出一个 JSON 对象（response_format 已要求 JSON），不要输出 Markdown 或解释文字。

任务要求：
1. is_actionable: 是否值得转入正式任务库。闲聊、纯情绪、无行动内容为 false。
2. kind: task(具体待办) / requirement(需求) / idea(想法) / note(纯记录)。
3. title: 不超过 40 个字的任务标题。
4. description: 用一句话把原始信息整理成可执行描述。
5. priority: P0/P1/P2/P3，缺省 P2。
6. deadline: 明确截止时间才填，格式 yyyy-MM-ddTHH:mm:ss+08:00；不确定或没有则 null。
7. estimate_minutes: 预估工时（分钟），不确定可 null。
8. tags: 2-4 个短标签。
9. subtasks: 如果原始信息包含多个动作，拆成子任务数组。
10. confidence: 0-1 之间的把握。
11. missing_info: 影响执行但缺失的信息列表，例如负责人、截止时间。
12. summary: 用一句话概括。

输出示例：
{
  "is_actionable": true,
  "kind": "requirement",
  "title": "导出功能增加部门筛选",
  "description": "在 OA 审批导出功能中增加按部门筛选的选项",
  "priority": "P1",
  "deadline": "2026-08-22T18:00:00+08:00",
  "estimate_minutes": 240,
  "tags": ["OA", "导出", "后端"],
  "subtasks": ["确认筛选字段", "修改导出接口", "前端联调"],
  "confidence": 0.9,
  "missing_info": ["负责人"],
  "summary": "需要为 OA 导出功能增加部门筛选"
}"#;

const IMAGE_SYSTEM_PROMPT: &str = r#"你是任务收件箱整理助手。用户发来一张截图或照片（可能附带一句话说明），请识别图中所有待办事项、任务、需求或别人交给用户办的事，输出一个 JSON 对象，不要输出 Markdown 或解释文字。

输出结构：
{"items": [待办对象, ...], "summary": "一句话概括整张图的内容"}

每个待办对象的字段：
1. is_actionable: 是否可行动
2. kind: task(具体待办) / requirement(需求) / idea(想法) / note(纯记录)
3. title: 不超过 40 字的标题
4. description: 一句话可执行描述，保留图中关键上下文（负责人、时间、系统名等）
5. priority: P0/P1/P2/P3，缺省 P2
6. deadline: 图中有明确截止时间才填，格式 yyyy-MM-ddTHH:mm:ss+08:00，否则 null
7. estimate_minutes: 预估分钟数，不确定 null
8. tags: 2-4 个短标签
9. subtasks: 需要时可拆子任务数组
10. confidence: 0-1 把握
11. missing_info: 影响执行但缺失的信息
12. summary: 一句话概括该条

要求：每个独立待办各占一项，聊天记录里不同人认领的事拆开；不要遗漏、不要臆造；图中没有可行动内容时 items 返回空数组。"#;

fn now_line() -> String {
    Local::now().format("当前时间：%Y-%m-%d %H:%M:%S %A").to_string()
}

fn user_prompt(raw_text: &str) -> String {
    format!(
        "{}\n\n请整理下面这条记录（原始文本不要改写，只在输出 JSON 中结构化它）：\n\n{raw_text}",
        now_line()
    )
}

fn image_user_prompt(raw_text: &str) -> String {
    let note = raw_text.trim();
    let note = if note.is_empty() { "（无）" } else { note };
    format!(
        "{}\n\n用户附言：{note}\n\n请识别并整理这张图片中的所有待办事项。",
        now_line()
    )
}

/// 按设置的协议分发到对应实现。带图片时走视觉模型（settings.vision_model，缺省用主模型）。
pub async fn parse_inbox_text(
    client: &reqwest::Client,
    settings: &Settings,
    raw_text: &str,
    image: Option<&ImageInput>,
) -> Result<AiParsed, AiError> {
    let api_key = settings.effective_api_key().ok_or(AiError::NoApiKey)?;
    if settings.provider() == PROVIDER_ANTHROPIC {
        parse_with_anthropic(client, settings, &api_key, raw_text, image).await
    } else {
        parse_with_openai(client, settings, &api_key, raw_text, image).await
    }
}

/// OpenAI Chat Completions 兼容协议（DeepSeek、Kimi、Qwen、GLM、Ollama 网关等）。
/// 文本走 json_object 模式；图片模式部分网关不支持 response_format，改为靠容错提取。
async fn parse_with_openai(
    client: &reqwest::Client,
    settings: &Settings,
    api_key: &str,
    raw_text: &str,
    image: Option<&ImageInput>,
) -> Result<AiParsed, AiError> {
    let with_image = image.is_some();
    let system = if with_image { IMAGE_SYSTEM_PROMPT } else { SYSTEM_PROMPT };
    let user_content = match image {
        Some(img) => json!([
            {"type": "text", "text": image_user_prompt(raw_text)},
            {"type": "image_url", "image_url": {"url": format!("data:{};base64,{}", img.mime, img.data)}}
        ]),
        None => json!(user_prompt(raw_text)),
    };

    let mut body = json!({
        "model": settings.model_for(with_image),
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user_content}
        ],
        "max_tokens": 1600,
        "stream": false
    });
    if !with_image {
        body["response_format"] = json!({"type": "json_object"});
    }

    let url = format!("{}/chat/completions", settings.normalized_base_url());
    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;

    let text = check_status(resp).await?;
    let value: Value = serde_json::from_str(&text)?;
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| AiError::Api(format!("返回结构不符合预期: {text}")))?;
    parse_content_json(content)
}

/// Anthropic Messages API。
async fn parse_with_anthropic(
    client: &reqwest::Client,
    settings: &Settings,
    api_key: &str,
    raw_text: &str,
    image: Option<&ImageInput>,
) -> Result<AiParsed, AiError> {
    let with_image = image.is_some();
    let system = if with_image { IMAGE_SYSTEM_PROMPT } else { SYSTEM_PROMPT };
    let user_content = match image {
        Some(img) => json!([
            {"type": "image", "source": {"type": "base64", "media_type": img.mime, "data": img.data}},
            {"type": "text", "text": image_user_prompt(raw_text)}
        ]),
        None => json!(user_prompt(raw_text)),
    };

    let body = json!({
        "model": settings.model_for(with_image),
        "max_tokens": 1600,
        "system": system,
        "messages": [
            {"role": "user", "content": user_content}
        ]
    });

    let url = format!("{}/v1/messages", settings.normalized_base_url());
    let resp = client
        .post(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await?;

    let text = check_status(resp).await?;
    let value: Value = serde_json::from_str(&text)?;
    let content = value
        .pointer("/content")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter()
                .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .and_then(|b| b.get("text").and_then(Value::as_str))
        })
        .ok_or_else(|| AiError::Api(format!("返回结构不符合预期: {text}")))?;
    parse_content_json(content)
}

async fn check_status(resp: reqwest::Response) -> Result<String, AiError> {
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AiError::Api(format!("HTTP {status}: {text}")));
    }
    Ok(text)
}

/// 容错提取模型输出中的 JSON：直接解析 → 剥离 ``` 代码块包裹 → 截取首个 {...} 平衡片段。
/// 图片模式没有 response_format 强约束，模型偶尔会加说明文字，这里保证尽量能取出对象。
fn parse_content_json(content: &str) -> Result<AiParsed, AiError> {
    if let Ok(p) = serde_json::from_str::<AiParsed>(content) {
        return Ok(p);
    }
    let trimmed = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(p) = serde_json::from_str::<AiParsed>(trimmed) {
        return Ok(p);
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if end > start {
            if let Ok(p) = serde_json::from_str::<AiParsed>(&trimmed[start..=end]) {
                return Ok(p);
            }
        }
    }
    Err(AiError::Api(format!(
        "返回内容不是合法 JSON: {}",
        content.chars().take(200).collect::<String>()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_content_json_tolerates_wrappers() {
        let p = parse_content_json(r#"{"title":"a","priority":"P1"}"#).unwrap();
        assert_eq!(p.title, "a");
        assert!(p.items.is_empty());

        // 图片模式的多待办包装结构
        let p = parse_content_json(
            r#"{"items":[{"title":"t1"},{"title":"t2"}],"summary":"s"}"#,
        )
        .unwrap();
        assert_eq!(p.items.len(), 2);
        assert_eq!(p.items[1].title, "t2");

        let p = parse_content_json("```json\n{\"title\":\"b\"}\n```").unwrap();
        assert_eq!(p.title, "b");

        let p = parse_content_json("好的，以下是结果：\n{\"title\":\"c\"}\n希望有帮助").unwrap();
        assert_eq!(p.title, "c");

        assert!(parse_content_json("今天天气不错").is_err());
    }

    #[test]
    fn sniff_and_encode_image() {
        let mut png = vec![0x89u8, b'P', b'N', b'G'];
        png.extend_from_slice(&[0u8; 16]);
        assert_eq!(sniff_image_mime(&png), Some("image/png"));
        let img = encode_image(&png).unwrap();
        assert_eq!(img.mime, "image/png");
        assert!(!img.data.is_empty());

        assert_eq!(sniff_image_mime(b"hello"), None);
        assert!(encode_image(b"hello").is_err());

        let jpeg = vec![0xFFu8, 0xD8, 0xFF, 0xE0];
        assert_eq!(sniff_image_mime(&jpeg), Some("image/jpeg"));

        // 超过上限快速失败
        let big = vec![0x89u8, b'P', b'N', b'G'].into_iter().chain(std::iter::repeat(0u8)).take(MAX_IMAGE_BYTES + 1).collect::<Vec<_>>();
        assert!(encode_image(&big).is_err());
    }
}
