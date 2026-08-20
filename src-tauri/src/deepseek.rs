use chrono::Local;
use serde_json::{json, Value};

use crate::models::AiParsed;
use crate::settings::Settings;

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("未配置 DeepSeek API Key")]
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

pub async fn parse_inbox_text(
    client: &reqwest::Client,
    settings: &Settings,
    raw_text: &str,
) -> Result<AiParsed, AiError> {
    let api_key = settings.effective_api_key().ok_or(AiError::NoApiKey)?;
    let now = Local::now().format("%Y-%m-%d %H:%M:%S %A").to_string();

    let user_prompt = format!(
        "当前时间：{now}\n\n请整理下面这条记录（原始文本不要改写，只在输出 JSON 中结构化它）：\n\n{raw_text}"
    );

    let body = json!({
        "model": settings.model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user_prompt}
        ],
        "response_format": {"type": "json_object"},
        "thinking": {"type": "disabled"},
        "max_tokens": 1200,
        "stream": false
    });

    let url = format!(
        "{}/chat/completions",
        settings.base_url.trim_end_matches('/')
    );
    let resp = client
        .post(&url)
        .bearer_auth(&api_key)
        .json(&body)
        .send()
        .await?;

    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AiError::Api(format!("HTTP {status}: {text}")));
    }

    let value: Value = serde_json::from_str(&text)?;
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| AiError::Api(format!("返回结构不符合预期: {text}")))?;

    let parsed: AiParsed = serde_json::from_str(content)?;
    Ok(parsed)
}
