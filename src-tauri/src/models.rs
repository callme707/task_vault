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
    /// True once an AI request has been explicitly queued and until its
    /// active generation completes.
    #[serde(default)]
    pub ai_requested: bool,
    /// Incremented when a worker claims or a stranded worker is recovered.
    #[serde(default)]
    pub ai_generation: i64,
    pub is_promoted: bool,
    /// Item keys already promoted from this inbox record.
    #[serde(default)]
    pub promoted_keys: Vec<String>,
    /// Stable keys for the currently stored AI item(s). Text/manual records
    /// expose one key; image results may expose one key per item.
    #[serde(default)]
    pub item_keys: Vec<String>,
    pub deleted_at: Option<String>,
    /// 随记录保存的截图/图片附件文件名，无图时为 None
    pub image_path: Option<String>,
}

/// AI 解析出的结构化结果。字段全部可选，避免模型少返回一个字段导致整条解析失败。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AiParsed {
    /// Jev-style System-1 routed expert persona ("dev" | "pm" | "gtd").
    #[serde(default)]
    pub persona: String,
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
    /// A parser may salvage a syntactically valid prefix from a truncated
    /// response. Such a result needs user review before promotion.
    #[serde(default)]
    pub partial: bool,
    /// 图片解析时模型可能一次返回多个待办；文本解析为空。
    /// 所有待办保留在同一条收件箱记录的 ai_result 中，由 item key 区分转正。
    #[serde(default)]
    pub items: Vec<AiParsed>,
}

/// 任务下的子任务清单。转正时来自 AI 解析结果，之后可增删、勾选完成。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subtask {
    pub id: i64,
    pub task_id: i64,
    pub text: String,
    pub is_done: bool,
    pub position: i64,
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
    /// 随任务列表一并带出的子任务；update_task 不处理该字段，走专门的子任务命令。
    #[serde(default)]
    pub subtasks: Vec<Subtask>,
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
    /// 转正时一并创建的子任务（来自 AI 解析，一行一条）。
    #[serde(default)]
    pub subtasks: Vec<String>,
}
