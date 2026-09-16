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
    #[error("AI 服务返回 HTTP {0}，请检查服务配置或稍后重试")]
    Http(u16),
    #[error("返回内容不是合法 JSON: {0}")]
    BadJson(#[from] serde_json::Error),
}

impl AiError {
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Network(_) | Self::Http(408 | 429 | 500..=599))
    }
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

/// 输出 token 上限。图片模式给足余量：推理型多模态模型（如 mimo/qwen-vl 系列）
/// 会先生成思维链再输出正文，推理 token 一并计入上限，给小了 JSON 会被拦腰截断。
/// MiMo 官方文档明确：max_completion_tokens 限制的是“思考+正文”总量，建议给足。
const TEXT_MAX_TOKENS: u32 = 2048;
const IMAGE_MAX_TOKENS: u32 = 16384;

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
请只输出一个 JSON 对象（response_format 已要求 JSON），不要输出 Markdown 或解释文字。输出为紧凑单行 JSON，不要缩进换行。

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

const IMAGE_SYSTEM_PROMPT: &str = r#"你的回复会被程序直接解析，硬性格式要求（违反则解析失败）：只输出一个紧凑单行 JSON 对象；禁止 Markdown 代码块、禁止缩进换行、禁止任何解释文字。

结构：{"items":[每个待办一个对象],"summary":"一句话概括整张图"}

每个待办对象只允许这些字段，用不到的必须省略（不要输出 null/空值）：
- title：不超过 40 字
- description：一句话可执行描述，保留图中关键上下文（负责人、时间、系统名）
- kind：task/requirement/idea/note
- priority：P0/P1/P2/P3，缺省 P2
- deadline：图中有明确截止时间才填，格式 yyyy-MM-ddTHH:mm:ss+08:00
- tags：短标签数组

识别要求：图中每个独立待办各占一项，聊天记录里不同人认领的事拆开；不要遗漏、不要臆造；没有可行动内容时 items 返回空数组。"#;

fn now_line() -> String {
    Local::now()
        .format("当前时间：%Y-%m-%d %H:%M:%S %A")
        .to_string()
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
    if !settings.api_ready() {
        return Err(AiError::NoApiKey);
    }
    let api_key = settings.effective_api_key().unwrap_or_default();
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
    let system = if with_image {
        IMAGE_SYSTEM_PROMPT
    } else {
        SYSTEM_PROMPT
    };
    let user_content = match image {
        Some(img) => json!([
            {"type": "text", "text": image_user_prompt(raw_text)},
            {"type": "image_url", "image_url": {"url": format!("data:{};base64,{}", img.mime, img.data)}}
        ]),
        None => json!(user_prompt(raw_text)),
    };

    // 文本模式用 max_tokens（DeepSeek 等传统兼容栈）；图片模式按 MiMo 等新栈
    // 文档只发 max_completion_tokens（限制“思考+正文”总量），避免端点在两个参数间取错
    let mut body = json!({
        "model": settings.model_for(with_image),
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user_content}
        ],
        "stream": false
    });
    if with_image {
        body["max_completion_tokens"] = json!(IMAGE_MAX_TOKENS);
    } else {
        body["max_tokens"] = json!(TEXT_MAX_TOKENS);
        body["response_format"] = json!({"type": "json_object"});
    }
    // 推理 token 计入输出上限，截断多因思维链烧完预算；不同服务商关思考的参数不同，
    // 三种常见写法同时携带，不认识的字段会被服务端忽略。严格校验的服务商可在设置中关掉。
    if with_image && settings.disable_reasoning {
        body["thinking"] = json!({"type": "disabled"});
        body["enable_thinking"] = json!(false);
        body["chat_template_kwargs"] = json!({"enable_thinking": false});
    }

    let url = format!("{}/chat/completions", settings.normalized_base_url());
    let mut request = client.post(&url).json(&body);
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    let resp = request.send().await?;

    let text = check_status(resp).await?;
    let value: Value = serde_json::from_str(&text)?;
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| AiError::Api("返回结构不符合预期，缺少文本结果".into()))?;
    match parse_content_json(content) {
        Ok(mut p) => {
            p.partial |= value
                .pointer("/choices/0/finish_reason")
                .and_then(Value::as_str)
                == Some("length");
            Ok(p)
        }
        // 附上响应元数据便于定位截断原因：finish_reason 与 token 用量
        Err(e) => Err(AiError::Api(format!("{e}｜{}", usage_line(&value)))),
    }
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
    let system = if with_image {
        IMAGE_SYSTEM_PROMPT
    } else {
        SYSTEM_PROMPT
    };
    let user_content = match image {
        Some(img) => json!([
            {"type": "image", "source": {"type": "base64", "media_type": img.mime, "data": img.data}},
            {"type": "text", "text": image_user_prompt(raw_text)}
        ]),
        None => json!(user_prompt(raw_text)),
    };

    let body = json!({
        "model": settings.model_for(with_image),
        "max_tokens": if with_image { IMAGE_MAX_TOKENS } else { TEXT_MAX_TOKENS },
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
        .ok_or_else(|| AiError::Api("返回结构不符合预期，缺少文本结果".into()))?;
    match parse_content_json(content) {
        Ok(mut p) => {
            p.partial |=
                value.pointer("/stop_reason").and_then(Value::as_str) == Some("max_tokens");
            Ok(p)
        }
        Err(e) => {
            let stop = value
                .pointer("/stop_reason")
                .and_then(Value::as_str)
                .unwrap_or("-");
            Err(AiError::Api(format!("{e}｜stop_reason={stop}")))
        }
    }
}

async fn check_status(resp: reqwest::Response) -> Result<String, AiError> {
    let status = resp.status();
    if !status.is_success() {
        // Error bodies can contain reflected request content or credentials.
        return Err(AiError::Http(status.as_u16()));
    }
    Ok(resp.text().await?)
}

/// 提取响应中的截断诊断信息：finish_reason 与 token 用量（含推理 token）。
/// 例：finish_reason=length, completion=8192 (reasoning=7900), prompt=1500
fn usage_line(value: &Value) -> String {
    let finish = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let completion = value
        .pointer("/usage/completion_tokens")
        .and_then(Value::as_i64);
    let reasoning = value
        .pointer("/usage/completion_tokens_details/reasoning_tokens")
        .and_then(Value::as_i64);
    let prompt = value
        .pointer("/usage/prompt_tokens")
        .and_then(Value::as_i64);
    format!(
        "finish_reason={finish}, completion={:?} (reasoning={:?}), prompt={:?}",
        completion, reasoning, prompt
    )
}

/// 部分 vLLM 自建栈不分离思维链，会把 <think>...</think> 直接混在 content 里。
fn strip_reasoning(content: &str) -> &str {
    if let Some(end) = content.rfind("</think>") {
        return &content[end + "</think>".len()..];
    }
    // 只有 <think> 开头且未闭合：全部是推理，正文没来得及输出
    if content.trim_start().starts_with("<think>") {
        return "";
    }
    content
}

/// 截断 JSON 抢救：截到最后一个完整值，去掉不完整的键值对，补齐未闭合括号。
/// 能把“输出到一半被掐断”里已完成的待办救回来。
fn salvage_json(s: &str) -> Option<AiParsed> {
    let scan_str_state = |head: &str| -> (bool, Option<usize>) {
        // 返回 (是否断在字符串内, 最后一个字符串外逗号的字节位置)
        let mut in_str = false;
        let mut esc = false;
        let mut last_comma = None;
        for (i, ch) in head.char_indices() {
            if in_str {
                if esc {
                    esc = false;
                } else if ch == '\\' {
                    esc = true;
                } else if ch == '"' {
                    in_str = false;
                }
            } else if ch == '"' {
                in_str = true;
            } else if ch == ',' {
                last_comma = Some(i);
            }
        }
        (in_str, last_comma)
    };

    // 有 '}' 截到其后；一个都没有（首个对象未闭合）则扫描全文，靠逗号回撤退掉残缺键值对
    let close_idx = s.rfind('}');
    let scan_end = close_idx.map_or(s.len(), |i| i + 1);
    let (in_str, last_comma) = scan_str_state(&s[..scan_end]);
    let mut end = scan_end;
    if in_str || close_idx.is_none() {
        end = last_comma?;
    }
    if end == 0 {
        return None;
    }
    let head = &s[..end];

    // 统计未闭合括号
    let mut stack = Vec::new();
    let mut in_str = false;
    let mut esc = false;
    for ch in head.chars() {
        if in_str {
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = false;
            }
        } else {
            match ch {
                '"' => in_str = true,
                '{' | '[' => stack.push(ch),
                '}' | ']' => {
                    stack.pop();
                }
                _ => {}
            }
        }
    }
    let mut fixed = head.to_string();
    while let Some(open) = stack.pop() {
        fixed.push(if open == '{' { '}' } else { ']' });
    }
    serde_json::from_str::<AiParsed>(&fixed).ok()
}

/// 容错提取模型输出中的 JSON：直接解析 → 剥离思考与 ``` 包裹 → 截取 {...} 片段 → 截断抢救。
fn parse_content_json(content: &str) -> Result<AiParsed, AiError> {
    let stripped = strip_reasoning(content);
    if stripped.trim().is_empty() {
        return Err(AiError::Api(
            "模型只输出了思考过程就被截断（推理耗尽了输出预算），请在设置中更换为非推理型视觉模型"
                .into(),
        ));
    }
    let trimmed = stripped
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    if let Ok(p) = serde_json::from_str::<AiParsed>(trimmed) {
        return validate_parsed(p);
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if end > start {
            if let Ok(p) = serde_json::from_str::<AiParsed>(&trimmed[start..=end]) {
                return validate_parsed(p);
            }
        }
    }
    // 输出被中途掐断时救回已完成的待办
    if let Some(start) = trimmed.find('{') {
        if let Some(mut p) = salvage_json(&trimmed[start..]) {
            p.partial = true;
            return validate_parsed(p);
        }
    }

    if !stripped.trim_end().ends_with('}') {
        return Err(AiError::Api(
            "输出未正常结束，可能被截断，无法恢复完整结果，请重试或更换模型。".into(),
        ));
    }
    Err(AiError::Api("AI 返回内容不是合法 JSON，请重新解析".into()))
}

fn validate_parsed(parsed: AiParsed) -> Result<AiParsed, AiError> {
    if parsed.title.trim().is_empty()
        && parsed.description.trim().is_empty()
        && parsed.summary.trim().is_empty()
        && parsed.items.is_empty()
    {
        return Err(AiError::Api("AI 返回了空结果，请重新解析".into()));
    }
    Ok(parsed)
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
        let p = parse_content_json(r#"{"items":[{"title":"t1"},{"title":"t2"}],"summary":"s"}"#)
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
    fn strips_inline_reasoning() {
        // vLLM 混排思维链：取 </think> 之后的内容
        let p =
            parse_content_json("<think>我先分析一下 {图片} 里的内容...</think>{\"title\":\"d\"}")
                .unwrap();
        assert_eq!(p.title, "d");
        // 只有未闭合的 <think>：判定为只有推理
        assert!(parse_content_json("<think>推理中...被截断").is_err());
    }

    #[test]
    fn salvages_truncated_items() {
        // 第二个待办被掐断：第一个应能救回
        let s = r#"{"items":[{"title":"a","kind":"task"},{"title":"b","deadli"#;
        let p = parse_content_json(s).unwrap();
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.items[0].title, "a");
        assert!(p.partial);

        // 断在字符串值中间
        let s = r#"{"items":[{"title":"a","deadline":"2026-09"#;
        let p = parse_content_json(s).unwrap();
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.items[0].title, "a");
        assert!(p.items[0].deadline.is_none());

        // 第一个对象都没闭合：救不回，报截断类错误
        let err = parse_content_json(r#"{"items":[{"title":"a"#).unwrap_err();
        assert!(err.to_string().contains("截断") || err.to_string().contains("JSON"));
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
        let big = vec![0x89u8, b'P', b'N', b'G']
            .into_iter()
            .chain(std::iter::repeat(0u8))
            .take(MAX_IMAGE_BYTES + 1)
            .collect::<Vec<_>>();
        assert!(encode_image(&big).is_err());
    }

    #[test]
    fn empty_model_objects_are_not_success() {
        assert!(parse_content_json("{}").is_err());
        assert!(parse_content_json(r#"{"unrecognized":"value"}"#).is_err());
        let error = parse_content_json("private-provider-response")
            .unwrap_err()
            .to_string();
        assert!(!error.contains("private-provider-response"));
    }

    fn fixture_server(status: &str, body: &str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let n = stream.read(&mut buffer).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..n]);
                if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(request).unwrap()
        });
        (address, worker)
    }

    #[tokio::test]
    async fn provider_truncation_marks_valid_json_as_partial() {
        let body = serde_json::json!({"choices":[{"message":{"content":"{\"title\":\"截断待办\"}"},"finish_reason":"length"}]}).to_string();
        let (base_url, server) = fixture_server("200 OK", &body);
        let settings = Settings {
            base_url,
            api_key: "fixture-key".into(),
            ..Settings::default()
        };
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let result = parse_inbox_text(&client, &settings, "合成测试记录", None)
            .await
            .unwrap();
        assert!(result.partial);
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /chat/completions"));
        assert!(request.contains("合成测试记录"));
    }

    #[tokio::test]
    async fn provider_error_does_not_store_the_response_body() {
        let (base_url, server) = fixture_server("401 Unauthorized", "fixture-private-response");
        let settings = Settings {
            base_url,
            api_key: "fixture-key".into(),
            ..Settings::default()
        };
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let error = parse_inbox_text(&client, &settings, "合成测试记录", None)
            .await
            .unwrap_err();
        assert!(matches!(error, AiError::Http(401)));
        assert!(!error.to_string().contains("fixture-private-response"));
        server.join().unwrap();
    }
}
