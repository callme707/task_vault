/* global window */
const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

const state = { inbox: [], tasks: [], selectedId: null, taskView: "active", taskFilter: "", editingId: null };

const dtToIso = (v) => (v ? new Date(v).toISOString() : null);

let toastTimer = null;
function showToast(message) {
  const el = document.getElementById("toast");
  if (!el) return;
  el.textContent = message;
  el.classList.remove("hidden");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.add("hidden"), 2200);
}

const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({
  "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
}[c]));

const statusText = {
  pending: "待解析", processing: "解析中", done: "已解析", failed: "失败",
  skipped: "跳过", unconfigured: "未配置 Key",
};

const taskStatusText = {
  todo: "待办", in_progress: "进行中", blocked: "阻塞", done: "完成",
};

function fmtDtLocal(v) {
  if (!v) return "";
  const d = new Date(v);
  if (Number.isNaN(d.getTime())) return v;
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

async function refreshInbox() {
  state.inbox = await invoke("list_inbox");
  renderInbox();
}

function renderInbox() {
  document.getElementById("inbox-count").textContent = `${state.inbox.length} 条`;
  const list = document.getElementById("inbox-list");
  if (!state.inbox.length) {
    list.innerHTML = `<div class="empty">还没有临时记录<br/>按 <b>Ctrl+Alt+Q</b> 快速记录</div>`;
    return;
  }
  list.innerHTML = state.inbox.map((it) => `
    <div class="item ${it.id === state.selectedId ? "active" : ""}" data-id="${it.id}">
      <div class="text">${esc(it.raw_text)}</div>
      <div class="meta">
        <span class="badge ${it.ai_status}">${statusText[it.ai_status] || it.ai_status}</span>
        ${it.is_promoted ? '<span class="badge promoted">已转正</span>' : ""}
        <span>${esc(it.created_at.slice(0, 16).replace("T", " "))}</span>
      </div>
    </div>`).join("");

  list.querySelectorAll(".item").forEach((el) => {
    el.onclick = () => selectInbox(Number(el.dataset.id));
  });
}

async function refreshTasks() {
  state.tasks = await invoke(
    state.taskView === "archived" ? "list_archived_tasks" : "list_tasks"
  );
  renderTasks();
}

function taskMatchesFilter(t, q) {
  const hay = `${t.title}\n${t.description}\n${t.tags.join(" ")}`.toLowerCase();
  return hay.includes(q);
}

function isOverdue(t) {
  return !!t.due_at && t.status !== "done" && new Date(t.due_at) < new Date();
}

function statusActionButtons(t) {
  const btn = (action, label) =>
    `<button class="btn small" data-action="${action}" data-id="${t.id}">${label}</button>`;
  switch (t.status) {
    case "todo": return btn("start", "开始");
    case "in_progress": return btn("done", "完成");
    case "blocked": return btn("start", "开始") + btn("done", "完成");
    case "done": return btn("reopen", "重开");
    default: return "";
  }
}

async function changeTaskStatus(id, status) {
  const t = state.tasks.find((x) => x.id === id);
  if (!t) return;
  try {
    await invoke("update_task", { task: { ...t, status } });
    showToast(`任务已${taskStatusText[status] || status}`);
  } catch (e) {
    alert(`状态更新失败：${e}`);
  }
}

function renderTasks() {
  const list = document.getElementById("tasks-list");
  const activeTab = document.getElementById("tab-active");
  const archivedTab = document.getElementById("tab-archived");
  if (activeTab) activeTab.classList.toggle("active", state.taskView === "active");
  if (archivedTab) archivedTab.classList.toggle("active", state.taskView === "archived");

  const q = state.taskFilter.trim().toLowerCase();
  const shown = q ? state.tasks.filter((t) => taskMatchesFilter(t, q)) : state.tasks;
  if (!shown.length) {
    list.innerHTML = `<div class="empty">${
      q ? "没有匹配的任务" : state.taskView === "archived"
        ? "暂无已归档任务"
        : '主数据库为空<br/>可从收件箱转正，或点上方“＋ 新建”直接添加'
    }</div>`;
    return;
  }

  const priorityOptions = (current) =>
    ["P0", "P1", "P2", "P3"]
      .map((p) => `<option value="${p}" ${p === current ? "selected" : ""}>${p}</option>`)
      .join("");

  list.innerHTML = shown.map((t) => {
    const overdue = isOverdue(t);
    const doneCount = (t.subtasks || []).filter((s) => s.is_done).length;
    return `
    <div class="task ${overdue ? "task-overdue" : ""}" data-id="${t.id}" title="点击卡片可编辑">
      <div class="task-head">
        <div class="title">${esc(t.title)}</div>
        <span class="badge st-${t.status}">${esc(taskStatusText[t.status] || t.status)}</span>
      </div>
      <div class="row">
        <span>优先级 <b class="prio p-${esc(t.priority)}">${esc(t.priority)}</b></span>
        ${t.due_at ? `<span class="${overdue ? "overdue" : ""}">截止 ${esc(t.due_at.slice(0, 10))}${overdue ? " · 已过期" : ""}</span>` : ""}
        ${t.estimate_minutes ? `<span>预估 ${t.estimate_minutes} 分钟</span>` : ""}
        ${t.actual_minutes ? `<span>实际 ${t.actual_minutes} 分钟</span>` : ""}
        ${t.planned_start ? `<span>${esc(t.planned_start.slice(0, 16))} ~ ${esc(t.planned_end ? t.planned_end.slice(0, 16) : "")}</span>` : ""}
        ${t.subtasks?.length ? `<span>子任务 ${doneCount}/${t.subtasks.length}</span>` : ""}
      </div>
      ${t.progress > 0 ? `<div class="progress"><div class="progress-bar" style="width:${Math.min(t.progress, 100)}%"></div></div>` : ""}
      <div class="task-actions">
        ${state.taskView === "archived" ? "" : statusActionButtons(t)}
        <label class="priority-field">
          优先级
          <select class="task-priority" data-id="${t.id}">${priorityOptions(t.priority)}</select>
        </label>
        ${
          state.taskView === "archived"
            ? `<button class="btn small" data-action="unarchive" data-id="${t.id}">恢复到进行中</button>`
            : `<button class="btn small" data-action="archive" data-id="${t.id}">归档</button>`
        }
        <button class="btn small danger-ghost" data-action="delete" data-id="${t.id}">删除</button>
      </div>
    </div>`;
  }).join("");

  list.querySelectorAll(".task").forEach((el) => {
    el.onclick = (e) => {
      if (e.target.closest("button, select, input, label")) return;
      openTaskModal(Number(el.dataset.id));
    };
  });

  list.querySelectorAll(".task-priority").forEach((el) => {
    el.onchange = async () => {
      try {
        await invoke("set_task_priority", { id: Number(el.dataset.id), priority: el.value });
        showToast("优先级已更新");
      } catch (e) {
        alert(`更新失败：${e}`);
      }
    };
  });

  const statusPatch = { start: "in_progress", done: "done", reopen: "todo" };
  Object.keys(statusPatch).forEach((action) => {
    list.querySelectorAll(`[data-action='${action}']`).forEach((el) => {
      el.onclick = () => changeTaskStatus(Number(el.dataset.id), statusPatch[action]);
    });
  });

  list.querySelectorAll("[data-action='archive']").forEach((el) => {
    el.onclick = async () => {
      if (!confirm("归档后将从当前列表移入“已归档”，可随时恢复。继续？")) return;
      await invoke("archive_task", { id: Number(el.dataset.id) });
      showToast("任务已归档");
    };
  });

  list.querySelectorAll("[data-action='unarchive']").forEach((el) => {
    el.onclick = async () => {
      await invoke("unarchive_task", { id: Number(el.dataset.id) });
      showToast("任务已恢复到进行中");
    };
  });

  list.querySelectorAll("[data-action='delete']").forEach((el) => {
    el.onclick = async () => {
      if (!confirm("确定删除该任务？删除后不会出现在任何列表中。")) return;
      await invoke("delete_task", { id: Number(el.dataset.id) });
      showToast("任务已删除");
    };
  });
}

function setTaskView(view) {
  if (view !== "active" && view !== "archived") return;
  state.taskView = view;
  renderTasks();
  refreshTasks();
}

/* ===== 任务编辑弹窗 ===== */

function getEditingTask() {
  return state.tasks.find((x) => x.id === state.editingId) || null;
}

function openTaskModal(id) {
  const t = state.tasks.find((x) => x.id === id);
  if (!t) return;
  state.editingId = id;
  setTaskModalMode("edit");
  document.getElementById("e-meta").textContent =
    `创建于 ${t.created_at.slice(0, 16).replace("T", " ")} · 来源 ${t.source_inbox_id ? `收件箱 #${t.source_inbox_id}` : "手动创建"}`;
  document.getElementById("e-title").value = t.title;
  document.getElementById("e-kind").value = t.kind;
  document.getElementById("e-status").value = t.status;
  document.getElementById("e-priority").value = t.priority;
  document.getElementById("e-due").value = fmtDtLocal(t.due_at);
  document.getElementById("e-estimate").value = t.estimate_minutes ?? "";
  document.getElementById("e-actual").value = t.actual_minutes;
  document.getElementById("e-progress").value = t.progress;
  document.getElementById("e-start").value = fmtDtLocal(t.planned_start);
  document.getElementById("e-end").value = fmtDtLocal(t.planned_end);
  document.getElementById("e-description").value = t.description;
  document.getElementById("e-tags").value = t.tags.join(", ");
  renderEditSubtasks();
  document.getElementById("task-modal").classList.remove("hidden");
}

function closeTaskModal() {
  state.editingId = null;
  document.getElementById("task-modal").classList.add("hidden");
}

// edit：改已有任务（子任务为即时操作的清单）；create：直接新建，子任务先按行填写，创建后转为清单。
function setTaskModalMode(mode) {
  const create = mode === "create";
  document.getElementById("e-modal-title").textContent = create ? "新建任务" : "编辑任务";
  document.getElementById("e-subtasks-zone").classList.toggle("hidden", create);
  document.getElementById("e-subtasks-new").classList.toggle("hidden", !create);
  document.getElementById("e-actual-field").classList.toggle("hidden", create);
  document.getElementById("e-progress-field").classList.toggle("hidden", create);
}

function openTaskModalForCreate() {
  state.editingId = null;
  setTaskModalMode("create");
  document.getElementById("e-meta").textContent = "直接创建正式任务，不经过收件箱";
  document.getElementById("e-title").value = "";
  document.getElementById("e-kind").value = "task";
  document.getElementById("e-status").value = "todo";
  document.getElementById("e-priority").value = "P2";
  document.getElementById("e-due").value = "";
  document.getElementById("e-estimate").value = "";
  document.getElementById("e-actual").value = "";
  document.getElementById("e-progress").value = "0";
  document.getElementById("e-start").value = "";
  document.getElementById("e-end").value = "";
  document.getElementById("e-description").value = "";
  document.getElementById("e-tags").value = "";
  document.getElementById("e-subtasks-new").value = "";
  document.getElementById("task-modal").classList.remove("hidden");
  document.getElementById("e-title").focus();
}

// 子任务操作即时落库，只重绘子任务区域，不动表单里未保存的其它修改。
function renderEditSubtasks() {
  const t = getEditingTask();
  const box = document.getElementById("e-subtasks");
  if (!t || !t.subtasks.length) {
    box.innerHTML = `<div class="subtask-empty">暂无子任务</div>`;
    return;
  }
  box.innerHTML = t.subtasks.map((s) => `
    <div class="subtask ${s.is_done ? "is-done" : ""}" data-id="${s.id}">
      <label class="subtask-check">
        <input type="checkbox" ${s.is_done ? "checked" : ""} />
        <span class="subtask-text">${esc(s.text)}</span>
      </label>
      <button class="btn small danger-ghost subtask-del" data-id="${s.id}">删除</button>
    </div>`).join("");

  box.querySelectorAll("input[type='checkbox']").forEach((el) => {
    el.onchange = async () => {
      try {
        await invoke("set_subtask_done", { id: Number(el.closest(".subtask").dataset.id), isDone: el.checked });
      } catch (e) {
        alert(`操作失败：${e}`);
      }
    };
  });
  box.querySelectorAll(".subtask-del").forEach((el) => {
    el.onclick = async () => {
      try {
        await invoke("delete_subtask", { id: Number(el.dataset.id) });
      } catch (e) {
        alert(`删除失败：${e}`);
      }
    };
  });
}

async function addSubtaskFromInput() {
  const input = document.getElementById("e-subtask-new");
  const text = input.value.trim();
  if (!text || !state.editingId) return;
  try {
    await invoke("add_subtask", { taskId: state.editingId, text });
    input.value = "";
  } catch (e) {
    alert(`添加失败：${e}`);
  }
}

async function saveTaskModal() {
  const title = document.getElementById("e-title").value.trim();
  if (!title) {
    alert("任务标题不能为空");
    return;
  }
  const num = (id, fallback) => {
    const v = document.getElementById(id).value;
    return v === "" ? fallback : Number(v);
  };
  const readDraft = () => ({
    title,
    description: document.getElementById("e-description").value.trim(),
    kind: document.getElementById("e-kind").value,
    status: document.getElementById("e-status").value,
    priority: document.getElementById("e-priority").value,
    due_at: dtToIso(document.getElementById("e-due").value),
    estimate_minutes: num("e-estimate", null),
    planned_start: dtToIso(document.getElementById("e-start").value),
    planned_end: dtToIso(document.getElementById("e-end").value),
    tags: document.getElementById("e-tags").value.split(/[,，]/).map((s) => s.trim()).filter(Boolean),
  });

  try {
    if (!state.editingId) {
      const draft = {
        ...readDraft(),
        subtasks: document.getElementById("e-subtasks-new").value.split("\n").map((s) => s.trim()).filter(Boolean),
      };
      await invoke("create_task", { draft });
      state.taskView = "active";
      closeTaskModal();
      showToast("任务已创建");
      return;
    }

    const t = getEditingTask();
    if (!t) {
      closeTaskModal();
      return;
    }
    const task = {
      ...t,
      ...readDraft(),
      actual_minutes: num("e-actual", 0) || 0,
      progress: Math.max(0, Math.min(100, num("e-progress", 0) || 0)),
    };
    await invoke("update_task", { task });
    closeTaskModal();
    showToast("任务已保存");
  } catch (e) {
    alert(`保存失败：${e}`);
  }
}

function parseAiResult(raw) {
  try { return raw ? JSON.parse(raw) : null; } catch { return null; }
}

function renderDetail(id) {
  const it = state.inbox.find((x) => x.id === id);
  if (!it) return;

  document.getElementById("detail-empty").classList.add("hidden");
  const box = document.getElementById("detail-content");
  box.classList.remove("hidden");
  document.getElementById("raw-text").textContent = it.raw_text;
  document.getElementById("ai-status").textContent = statusText[it.ai_status] || it.ai_status;
  document.getElementById("ai-status").className = `badge ${it.ai_status}`;
  document.getElementById("ai-model").textContent = it.ai_model ? `模型：${it.ai_model}` : "";

  const ai = parseAiResult(it.ai_result);
  const aiBox = document.getElementById("ai-summary");
  if (it.ai_status === "failed") {
    aiBox.textContent = `解析失败：${it.ai_result || "未知错误"}\n可点“重新解析”重试，多次失败请检查网络或 API Key。`;
  } else if (ai) {
    aiBox.textContent = [
      `类型：${ai.kind || "-"}  优先级：${ai.priority || "-"}  把握：${ai.confidence ?? "-"}`,
      ai.summary ? `总结：${ai.summary}` : "",
      ai.deadline ? `截止：${ai.deadline}` : "",
      ai.estimate_minutes ? `预估：${ai.estimate_minutes} 分钟` : "",
      ai.tags?.length ? `标签：${ai.tags.join(", ")}` : "",
      ai.subtasks?.length ? `子任务：${ai.subtasks.join("；")}` : "",
      ai.missing_info?.length ? `待补充：${ai.missing_info.join("；")}` : "",
    ].filter(Boolean).join("\n");
  } else {
    aiBox.textContent = it.ai_status === "unconfigured"
      ? "未配置 DeepSeek API Key，点右上角“设置”填写后重新解析。"
      : "暂无 AI 解析结果。";
  }

  document.getElementById("f-title").value = ai?.title || it.raw_text.slice(0, 40);
  document.getElementById("f-kind").value = ai?.kind || "task";
  document.getElementById("f-priority").value = ai?.priority || "P2";
  document.getElementById("f-status").value = "todo";
  document.getElementById("f-due").value = fmtDtLocal(ai?.deadline);
  document.getElementById("f-estimate").value = ai?.estimate_minutes ?? "";
  document.getElementById("f-start").value = "";
  document.getElementById("f-end").value = "";
  document.getElementById("f-description").value = ai?.description || it.raw_text;
  document.getElementById("f-tags").value = (ai?.tags || []).join(", ");
  document.getElementById("f-subtasks").value = (ai?.subtasks || []).join("\n");
}

function selectInbox(id) {
  state.selectedId = id;
  renderInbox();
  renderDetail(id);
}

async function promoteSelected() {
  if (!state.selectedId) return;
  const draft = {
    title: document.getElementById("f-title").value.trim(),
    description: document.getElementById("f-description").value.trim(),
    kind: document.getElementById("f-kind").value,
    status: document.getElementById("f-status").value,
    priority: document.getElementById("f-priority").value,
    due_at: dtToIso(document.getElementById("f-due").value),
    estimate_minutes: document.getElementById("f-estimate").value ? Number(document.getElementById("f-estimate").value) : null,
    planned_start: dtToIso(document.getElementById("f-start").value),
    planned_end: dtToIso(document.getElementById("f-end").value),
    tags: document.getElementById("f-tags").value.split(/[,，]/).map((s) => s.trim()).filter(Boolean),
    subtasks: document.getElementById("f-subtasks").value.split("\n").map((s) => s.trim()).filter(Boolean),
  };
  try {
    await invoke("promote_inbox", { inboxId: state.selectedId, draft });
    state.selectedId = null;
    document.getElementById("detail-content").classList.add("hidden");
    document.getElementById("detail-empty").classList.remove("hidden");
    await refreshAll();
  } catch (e) {
    alert(`转正失败：${e}`);
  }
}

async function refreshAll() {
  await Promise.all([refreshInbox(), refreshTasks()]);
  if (state.selectedId) renderDetail(state.selectedId);
  // 编辑弹窗打开时，子任务操作走即时落库 + tasks-updated 事件，这里只刷新子任务区域。
  if (state.editingId) renderEditSubtasks();
}

let currentSettings = null;

// 常用服务商预设：切换时自动填协议/地址/模型，用户可再手改覆盖。
const PRESETS = {
  deepseek:   { provider: "openai",    base_url: "https://api.deepseek.com",             model: "deepseek-v4-flash" },
  openai:     { provider: "openai",    base_url: "https://api.openai.com/v1",            model: "gpt-4o-mini" },
  moonshot:   { provider: "openai",    base_url: "https://api.moonshot.cn/v1",           model: "kimi-latest" },
  qwen:       { provider: "openai",    base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "qwen-plus" },
  glm:        { provider: "openai",    base_url: "https://open.bigmodel.cn/api/paas/v4", model: "glm-4-flash" },
  ollama:     { provider: "openai",    base_url: "http://localhost:11434/v1",            model: "qwen3:8b" },
  anthropic:  { provider: "anthropic", base_url: "https://api.anthropic.com",            model: "claude-sonnet-4-5" },
};

async function loadSettings() {
  currentSettings = await invoke("get_settings");
  document.getElementById("s-provider").value = currentSettings.provider || "openai";
  document.getElementById("s-api-key").value = currentSettings.api_key || "";
  document.getElementById("s-base-url").value = currentSettings.base_url;
  document.getElementById("s-model").value = currentSettings.model;
  document.getElementById("s-auto-parse").checked = !!currentSettings.auto_parse;
  document.getElementById("s-autostart").checked = !!currentSettings.autostart;
}

async function saveSettings() {
  const s = {
    api_key: document.getElementById("s-api-key").value.trim(),
    provider: document.getElementById("s-provider").value,
    base_url: document.getElementById("s-base-url").value.trim() || "https://api.deepseek.com",
    model: document.getElementById("s-model").value.trim() || "deepseek-v4-flash",
    auto_parse: document.getElementById("s-auto-parse").checked,
    autostart: document.getElementById("s-autostart").checked,
    // 快捷键暂未开放界面编辑，保留后端当前值，避免保存时被重置回默认。
    quick_hotkey: currentSettings?.quick_hotkey || "Ctrl+Alt+Q",
    main_hotkey: currentSettings?.main_hotkey || "Ctrl+Alt+W",
  };
  try {
    await invoke("save_settings", { settings: s });
    showToast("设置已保存");
  } catch (e) {
    // 后端报错时设置通常已写入（如自启开关操作失败），文案自带说明
    alert(String(e));
  }
  document.getElementById("settings-modal").classList.add("hidden");
}

document.addEventListener("DOMContentLoaded", async () => {
  document.getElementById("btn-refresh").onclick = refreshAll;
  document.getElementById("btn-process-pending").onclick = async () => {
    try {
      const n = await invoke("process_pending_inbox");
      if (n) showToast(`已启动 ${n} 条记录的 AI 解析`);
    } catch (e) { alert(e); }
  };
  document.getElementById("tab-active").onclick = () => setTaskView("active");
  document.getElementById("tab-archived").onclick = () => setTaskView("archived");
  document.getElementById("btn-settings").onclick = async () => {
    await loadSettings();
    document.getElementById("settings-modal").classList.remove("hidden");
  };
  document.getElementById("s-preset").onchange = (e) => {
    const p = PRESETS[e.target.value];
    if (!p) return;
    document.getElementById("s-provider").value = p.provider;
    document.getElementById("s-base-url").value = p.base_url;
    document.getElementById("s-model").value = p.model;
  };
  document.getElementById("btn-close-settings").onclick = () =>
    document.getElementById("settings-modal").classList.add("hidden");
  document.getElementById("btn-save-settings").onclick = saveSettings;
  document.getElementById("btn-promote").onclick = promoteSelected;
  document.getElementById("btn-close-task").onclick = closeTaskModal;
  document.getElementById("btn-save-task").onclick = saveTaskModal;
  document.getElementById("btn-new-task").onclick = openTaskModalForCreate;
  document.getElementById("e-subtask-add").onclick = addSubtaskFromInput;
  document.getElementById("e-subtask-new").addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      addSubtaskFromInput();
    }
  });
  document.getElementById("task-filter").addEventListener("input", (e) => {
    state.taskFilter = e.target.value;
    renderTasks();
  });
  document.getElementById("btn-delete-inbox").onclick = async () => {
    if (!state.selectedId) return;
    if (!confirm("只会在临时库中软删除，不会删除主库关联记录。继续？")) return;
    await invoke("delete_inbox", { id: state.selectedId });
    state.selectedId = null;
    document.getElementById("detail-content").classList.add("hidden");
    document.getElementById("detail-empty").classList.remove("hidden");
    await refreshAll();
  };
  document.getElementById("btn-reparse").onclick = async () => {
    if (!state.selectedId) return;
    await invoke("parse_inbox_item", { id: state.selectedId });
  };

  await listen("inbox-updated", () => refreshAll());
  await listen("tasks-updated", () => refreshAll());

  await refreshAll();
});
