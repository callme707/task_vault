use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct InboxItem {
    pub id: i64,
    pub raw_text: String,
    pub source: String,
    pub created_at: String,
    pub ai_status: String,
    pub ai_result: Option<String>,
    pub ai_model: Option<String>,
    pub is_promoted: bool,
    pub deleted_at: Option<String>,
}

/// AI 解析出的结构化结果。字段全部可选，避免模型少返回一个字段导致整条解析失败。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AiParsed {
    #[serde(default)]
    pub is_actionable: bool,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub priority: String,
    #[serde(default)]
    pub deadline: Option<String>,
    #[serde(default)]
    pub estimate_minutes: Option<i64>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub subtasks: Vec<String>,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub missing_info: Vec<String>,
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub kind: String,
    pub status: String,
    pub priority: String,
    pub due_at: Option<String>,
    pub estimate_minutes: Option<i64>,
    pub actual_minutes: i64,
    pub progress: i64,
    pub planned_start: Option<String>,
    pub planned_end: Option<String>,
    pub tags: Vec<String>,
    pub source_inbox_id: Option<i64>,
    pub source_snapshot: Option<String>,
    #[serde(default)]
    pub is_archived: bool,
    #[serde(default)]
    pub deleted_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 主窗口提交的“转正”草稿。用户可直接使用 AI 草稿，也可在界面上修改后提交。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDraft {
    pub title: String,
    pub description: String,
    pub kind: String,
    pub status: String,
    pub priority: String,
    pub due_at: Option<String>,
    pub estimate_minutes: Option<i64>,
    pub planned_start: Option<String>,
    pub planned_end: Option<String>,
    pub tags: Vec<String>,
}
