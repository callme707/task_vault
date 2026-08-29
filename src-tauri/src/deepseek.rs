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

fn user_prompt(raw_text: &str) -> String {
    let now = Local::now().format("%Y-%m-%d %H:%M:%S %A").to_string();
    format!(
        "当前时间：{now}\n\n请整理下面这条记录（原始文本不要改写，只在输出 JSON 中结构化它）：\n\n{raw_text}"
    )
}

/// 按设置的协议分发到对应实现。返回结构化解析结果。
pub async fn parse_inbox_text(
    client: &reqwest::Client,
    settings: &Settings,
    raw_text: &str,
) -> Result<AiParsed, AiError> {
    let api_key = settings.effective_api_key().ok_or(AiError::NoApiKey)?;
    if settings.provider() == PROVIDER_ANTHROPIC {
        parse_with_anthropic(client, settings, &api_key, raw_text).await
    } else {
        parse_with_openai(client, settings, &api_key, raw_text).await
    }
}

/// OpenAI Chat Completions 兼容协议（DeepSeek、Kimi、Qwen、GLM、Ollama 网关等）。
/// 要求 JSON 输出（json_object 模式或网关自动剥离 ``` 包裹）。
async fn parse_with_openai(
    client: &reqwest::Client,
    settings: &Settings,
    api_key: &str,
    raw_text: &str,
) -> Result<AiParsed, AiError> {
    let body = json!({
        "model": settings.model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user_prompt(raw_text)}
        ],
        "response_format": {"type": "json_object"},
        "max_tokens": 1200,
        "stream": false
    });

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
/// 无 response_format，靠 system prompt 约束 + 容错提取 JSON 文本。
async fn parse_with_anthropic(
    client: &reqwest::Client,
    settings: &Settings,
    api_key: &str,
    raw_text: &str,
) -> Result<AiParsed, AiError> {
    let body = json!({
        "model": settings.model,
        "max_tokens": 1200,
        "system": SYSTEM_PROMPT,
        "messages": [
            {"role": "user", "content": user_prompt(raw_text)}
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
    // Messages 返回 content 数组，逐块找 text
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
/// Anthropic 无强制 JSON 模式，模型偶尔会加说明文字，这里保证尽量能取出对象。
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
        // 裸 JSON
        let p = parse_content_json(r#"{"title":"a","priority":"P1"}"#).unwrap();
        assert_eq!(p.title, "a");
        // ```json 包裹
        let p = parse_content_json("```json\n{\"title\":\"b\"}\n```").unwrap();
        assert_eq!(p.title, "b");
        // 前后带说明文字
        let p = parse_content_json("好的，以下是结果：\n{\"title\":\"c\"}\n希望有帮助").unwrap();
        assert_eq!(p.title, "c");
        // 确实不是 JSON
        assert!(parse_content_json("今天天气不错").is_err());
    }
}
