/* global window */
const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

const state = { inbox: [], tasks: [], selectedId: null, taskView: "active" };

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

function renderTasks() {
  const list = document.getElementById("tasks-list");
  const activeTab = document.getElementById("tab-active");
  const archivedTab = document.getElementById("tab-archived");
  if (activeTab) activeTab.classList.toggle("active", state.taskView === "active");
  if (archivedTab) archivedTab.classList.toggle("active", state.taskView === "archived");

  if (!state.tasks.length) {
    list.innerHTML = `<div class="empty">${
      state.taskView === "archived" ? "暂无已归档任务" : "主数据库为空"
    }</div>`;
    return;
  }

  const priorityOptions = (current) =>
    ["P0", "P1", "P2", "P3"]
      .map((p) => `<option value="${p}" ${p === current ? "selected" : ""}>${p}</option>`)
      .join("");

  list.innerHTML = state.tasks.map((t) => `
    <div class="task" data-id="${t.id}">
      <div class="task-head">
        <div class="title">${esc(t.title)}</div>
        <span class="badge">${esc(taskStatusText[t.status] || t.status)}</span>
      </div>
      <div class="row">
        <span>优先级 ${esc(t.priority)}</span>
        ${t.due_at ? `<span>截止 ${esc(t.due_at.slice(0, 10))}</span>` : ""}
        ${t.estimate_minutes ? `<span>预估 ${t.estimate_minutes} 分钟</span>` : ""}
        ${t.planned_start ? `<span>${esc(t.planned_start.slice(0, 16))} ~ ${esc(t.planned_end ? t.planned_end.slice(0, 16) : "")}</span>` : ""}
      </div>
      <div class="task-actions">
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
    </div>`).join("");

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
  if (ai) {
    aiBox.textContent = [
      `类型：${ai.kind || "-"}  优先级：${ai.priority || "-"}  把握：${ai.confidence ?? "-"}`,
      ai.summary ? `总结：${ai.summary}` : "",
      ai.deadline ? `截止：${ai.deadline}` : "",
      ai.estimate_minutes ? `预估：${ai.estimate_minutes} 分钟` : "",
      ai.tags?.length ? `标签：${ai.tags.join(", ")}` : "",
      ai.subtasks?.length ? `子任务：${ai.subtasks.join("；")}` : "",
      ai.missing_info?.length ? `待补充：${ai.missing_info.join("；")}` : "",
      it.ai_status === "failed" ? `错误：${it.ai_result}` : "",
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
    due_at: document.getElementById("f-due").value ? new Date(document.getElementById("f-due").value).toISOString() : null,
    estimate_minutes: document.getElementById("f-estimate").value ? Number(document.getElementById("f-estimate").value) : null,
    planned_start: document.getElementById("f-start").value ? new Date(document.getElementById("f-start").value).toISOString() : null,
    planned_end: document.getElementById("f-end").value ? new Date(document.getElementById("f-end").value).toISOString() : null,
    tags: document.getElementById("f-tags").value.split(/[,，]/).map((s) => s.trim()).filter(Boolean),
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
}

async function loadSettings() {
  const s = await invoke("get_settings");
  document.getElementById("s-api-key").value = s.api_key || "";
  document.getElementById("s-base-url").value = s.base_url;
  document.getElementById("s-model").value = s.model;
  document.getElementById("s-auto-parse").checked = !!s.auto_parse;
  document.getElementById("s-autostart").checked = !!s.autostart;
}

async function saveSettings() {
  const s = {
    api_key: document.getElementById("s-api-key").value.trim(),
    base_url: document.getElementById("s-base-url").value.trim(),
    model: document.getElementById("s-model").value.trim() || "deepseek-v4-flash",
    auto_parse: document.getElementById("s-auto-parse").checked,
    autostart: document.getElementById("s-autostart").checked,
    quick_hotkey: "Ctrl+Alt+Q",
    main_hotkey: "Ctrl+Alt+W",
  };
  try {
    await invoke("save_settings", { settings: s });
    document.getElementById("settings-modal").classList.add("hidden");
    showToast("设置已保存");
  } catch (e) {
    alert(`保存失败：${e}`);
  }
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
  document.getElementById("btn-close-settings").onclick = () =>
    document.getElementById("settings-modal").classList.add("hidden");
  document.getElementById("btn-save-settings").onclick = saveSettings;
  document.getElementById("btn-promote").onclick = promoteSelected;
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
