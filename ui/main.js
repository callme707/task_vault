/* global window, document */
"use strict";

// Keep the file usable by the static Tauri page and by small Node regression tests.
const tauri = typeof window !== "undefined" ? window.__TAURI__ : null;
const invoke = tauri?.core?.invoke
  ? (...args) => tauri.core.invoke(...args)
  : async () => { throw new Error("Tauri API 不可用"); };
const listen = tauri?.event?.listen
  ? (...args) => tauri.event.listen(...args)
  : null;

const state = {
  inbox: [],
  tasks: [],
  taskBuckets: { active: [], completed: [], archived: [] },
  taskCounts: { active: 0, completed: 0, archived: 0 },
  taskRefreshToken: 0,
  selectedId: null,
  selectedItemIndex: 0,
  taskView: "active",
  taskFilter: "",
  editingId: null,
  view: "inbox",
  inboxDrafts: new Map(),
  busy: new Set(),
  refreshPromise: null,
  detailRenderToken: 0,
  modalId: null,
  modalLastFocus: null,
};

const statusText = {
  queued: "排队中",
  pending: "待解析",
  processing: "解析中",
  done: "已解析",
  partial: "结果待核对",
  failed: "解析失败",
  skipped: "已跳过",
  unconfigured: "未配置 Key",
};

const taskStatusText = {
  todo: "待办",
  in_progress: "进行中",
  blocked: "阻塞",
  done: "完成",
};

const taskViews = ["active", "completed", "archived"];

function isTaskArchived(task) {
  return task?.is_archived === true || task?.is_archived === 1 || task?.is_archived === "1" || task?.is_archived === "true";
}

function taskViewFor(task) {
  if (isTaskArchived(task)) return "archived";
  return task?.status === "done" ? "completed" : "active";
}

function classifyTasks(tasks) {
  const buckets = { active: [], completed: [], archived: [] };
  safeArray(tasks).forEach((task) => buckets[taskViewFor(task)].push(task));
  return buckets;
}

function mergeTaskSources(...sources) {
  const byId = new Map();
  const withoutId = [];
  sources.forEach((source) => {
    safeArray(source).forEach((task) => {
      if (task?.id == null) {
        withoutId.push(task);
        return;
      }
      const key = String(task.id);
      const existing = byId.get(key);
      // If a task moved between the two queries, prefer the archived record so
      // one task cannot be counted in both scopes during that transition.
      if (!existing || isTaskArchived(task)) byId.set(key, task);
    });
  });
  return [...byId.values(), ...withoutId];
}

function taskAfterAction(task, action) {
  if (!task || typeof task !== "object") return task;
  if (action === "start") return { ...task, status: "in_progress" };
  if (action === "done") return { ...task, status: "done" };
  if (action === "reopen" && task.status === "done") return { ...task, status: "todo" };
  if (action === "archive") return { ...task, is_archived: true };
  if (action === "unarchive") return { ...task, is_archived: false };
  return { ...task };
}

const viewCopy = {
  inbox: {
    kicker: "RECORDS",
    title: "记录区",
    subtitle: "先收集，再确认。AI 建议会保留在原始记录旁。",
  },
  tasks: {
    kicker: "TASKS",
    title: "正式任务",
    subtitle: "把确认过的内容放进正式任务，持续推进到完成。",
  },
};

let toastTimer = null;
let currentSettings = null;

/* ===== 时间：UTC 存储，显示统一走 Intl 的本地时区 ===== */
function asDate(value) {
  if (!value) return null;
  const date = value instanceof Date ? value : new Date(value);
  return Number.isNaN(date.getTime()) ? null : date;
}

function localDateParts(value) {
  const date = asDate(value);
  if (!date) return null;
  const parts = new Intl.DateTimeFormat(undefined, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).formatToParts(date);
  const result = {};
  for (const part of parts) {
    if (part.type !== "literal") result[part.type] = part.value;
  }
  return result;
}

function formatDateTime(value) {
  const p = localDateParts(value);
  return p ? `${p.year}-${p.month}-${p.day} ${p.hour}:${p.minute}` : "";
}

function formatDateOnly(value) {
  const p = localDateParts(value);
  return p ? `${p.year}-${p.month}-${p.day}` : "";
}

function formatTime(value) {
  const p = localDateParts(value);
  return p ? `${p.hour}:${p.minute}` : "";
}

function fmtDtLocal(value) {
  const p = localDateParts(value);
  return p ? `${p.year}-${p.month}-${p.day}T${p.hour}:${p.minute}` : "";
}

function dtToIso(value) {
  if (!value) return null;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
}

/* ===== 通用 UI 工具 ===== */
const esc = (value) => String(value ?? "").replace(/[&<>"']/g, (c) => ({
  "&": "&amp;",
  "<": "&lt;",
  ">": "&gt;",
  '"': "&quot;",
  "'": "&#39;",
}[c]));

function errorText(error) {
  if (error == null) return "未知错误";
  if (typeof error === "string") return error;
  if (error.message) return String(error.message);
  return String(error);
}

function showToast(message, kind = "success") {
  const el = document.getElementById("toast");
  if (!el) return;
  el.textContent = message;
  el.classList.toggle("error", kind === "error");
  el.classList.remove("hidden");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.add("hidden"), kind === "error" ? 4200 : 2500);
}

function showError(message) {
  showToast(message, "error");
}

function setButtonBusy(id, busy, label, ownerKey) {
  const button = document.getElementById(id);
  if (!button) return false;
  if (busy) {
    const currentOwner = button.dataset.busyKey;
    if (currentOwner && currentOwner !== ownerKey) return false;
    if (ownerKey) button.dataset.busyKey = ownerKey;
    if (!button.dataset.idleLabel) button.dataset.idleLabel = button.textContent;
    button.disabled = true;
    button.setAttribute("aria-busy", "true");
    if (label) button.textContent = label;
    return true;
  } else {
    if (ownerKey && button.dataset.busyKey !== ownerKey) return false;
    button.disabled = false;
    button.removeAttribute("aria-busy");
    if (button.dataset.idleLabel) button.textContent = button.dataset.idleLabel;
    delete button.dataset.busyKey;
    delete button.dataset.idleLabel;
    return true;
  }
}

async function runBusy(key, fn, buttonId, busyLabel, onIdle) {
  if (state.busy.has(key)) return undefined;
  state.busy.add(key);
  if (buttonId && !setButtonBusy(buttonId, true, busyLabel, key)) {
    state.busy.delete(key);
    return undefined;
  }
  try {
    return await fn();
  } finally {
    state.busy.delete(key);
    const released = buttonId ? setButtonBusy(buttonId, false, undefined, key) : true;
    if (released) onIdle?.();
  }
}

function safeArray(value) {
  return Array.isArray(value) ? value : [];
}

function parseAiResult(raw) {
  if (!raw || typeof raw !== "string") return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function aiItemsFor(inboxItem) {
  const ai = parseAiResult(inboxItem?.ai_result);
  if (Array.isArray(ai?.items) && ai.items.length) return ai.items;
  return ai ? [ai] : [];
}

function hasAiItemList(inboxItem) {
  const ai = parseAiResult(inboxItem?.ai_result);
  return Array.isArray(ai?.items) && ai.items.length > 0;
}

function itemKeyFor(inboxItem, index) {
  const keys = safeArray(inboxItem?.item_keys);
  const key = keys[index];
  if (typeof key === "string" && key.trim()) return key;
  // Plain text records have one implicit backend item. Image items must wait for
  // the backend-generated stable key; the UI never invents an image key.
  if (!inboxItem?.image_path && index === 0 && !hasAiItemList(inboxItem)) return "default";
  return null;
}

function promotedKeysFor(inboxItem) {
  if (Array.isArray(inboxItem?.promoted_keys)) return inboxItem.promoted_keys.map(String);
  if (Array.isArray(inboxItem?.promoted_items)) return inboxItem.promoted_items.map(String);
  return [];
}

function isItemPromoted(inboxItem, index) {
  const items = aiItemsFor(inboxItem);
  const key = itemKeyFor(inboxItem, index);
  if (key && promotedKeysFor(inboxItem).includes(String(key))) return true;
  // Legacy single-item records only had the boolean marker.
  if (!inboxItem?.image_path && items.length <= 1 && !safeArray(inboxItem?.item_keys).length) {
    return !!inboxItem?.is_promoted;
  }
  return false;
}

function promotionCounts(inboxItem) {
  const items = aiItemsFor(inboxItem);
  const total = hasAiItemList(inboxItem) ? items.length : 1;
  let promoted = 0;
  for (let i = 0; i < total; i += 1) {
    if (isItemPromoted(inboxItem, i)) promoted += 1;
  }
  return { promoted, total };
}

function sourceText(source) {
  const labels = { hotkey: "快速记录", main: "工作台", clipboard: "剪贴板", image: "图片记录" };
  return labels[source] || source || "本地记录";
}

/* ===== 导航与列表 ===== */
function switchView(view) {
  if (!viewCopy[view]) return;
  state.view = view;
  document.querySelectorAll(".nav-item[data-view]").forEach((button) => {
    const active = button.dataset.view === view;
    button.classList.toggle("active", active);
    button.setAttribute("aria-current", active ? "page" : "false");
  });
  document.getElementById("view-inbox")?.classList.toggle("hidden", view !== "inbox");
  document.getElementById("view-tasks")?.classList.toggle("hidden", view !== "tasks");
  document.getElementById("btn-process-pending")?.classList.toggle("hidden", view !== "inbox");
  document.getElementById("page-kicker").textContent = viewCopy[view].kicker;
  document.getElementById("page-title").textContent = viewCopy[view].title;
  document.getElementById("page-subtitle").textContent = viewCopy[view].subtitle;
}

function renderInbox() {
  const list = document.getElementById("inbox-list");
  const count = document.getElementById("inbox-count");
  const navCount = document.getElementById("nav-inbox-count");
  if (count) count.textContent = `${state.inbox.length} 条记录`;
  if (navCount) navCount.textContent = String(state.inbox.length);
  if (!list) return;
  if (!state.inbox.length) {
    list.innerHTML = '<div class="empty">还没有临时记录<br /><strong>按 Ctrl + Alt + Q 快速记录</strong></div>';
    return;
  }

  list.innerHTML = state.inbox.map((item) => {
    const { promoted, total } = promotionCounts(item);
    const promotion = promoted
      ? `<span class="badge promoted">${promoted}/${total} 项已转为正式任务</span>`
      : hasAiItemList(item) ? `<span class="row-count">${total} 个待办</span>` : "";
    const imageMark = item.image_path ? '<span class="image-label">图片</span>' : "";
    const title = item.raw_text || (item.image_path ? "图片记录" : "无文字内容");
    const active = item.id === state.selectedId;
    return `
      <article class="inbox-row ${active ? "active" : ""}" data-id="${item.id}" tabindex="0" role="button" aria-label="打开记录：${esc(title)}">
        <div class="inbox-row-main">
          <div class="inbox-row-kicker"><span>${esc(sourceText(item.source))}</span><time datetime="${esc(item.created_at)}">${esc(formatDateTime(item.created_at))}</time></div>
          <div class="inbox-row-title">${imageMark}${esc(title)}</div>
          <div class="inbox-row-meta"><span class="badge ${esc(item.ai_status || "pending")}">${esc(statusText[item.ai_status] || item.ai_status || "未知状态")}</span>${promotion}</div>
        </div>
      </article>`;
  }).join("");

  list.querySelectorAll(".inbox-row").forEach((row) => {
    const select = () => selectInbox(Number(row.dataset.id));
    row.addEventListener("click", select);
    row.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        select();
      }
    });
  });
}

function taskMatchesFilter(task, query) {
  const haystack = `${task.title || ""}\n${task.description || ""}\n${safeArray(task.tags).join(" ")}`.toLowerCase();
  return haystack.includes(query);
}

function isOverdue(task) {
  return !!task.due_at && task.status !== "done" && asDate(task.due_at)?.getTime() < Date.now();
}

function statusActionButtons(task) {
  const button = (action, label) => `<button class="btn small" type="button" data-action="${action}" data-id="${task.id}">${label}</button>`;
  if (task.status === "todo") return button("start", "开始") + button("done", "完成");
  if (task.status === "in_progress") return button("done", "完成");
  if (task.status === "blocked") return button("start", "开始") + button("done", "完成");
  if (task.status === "done") return button("reopen", "重开");
  return "";
}

function renderTasks() {
  const list = document.getElementById("task-list") || document.getElementById("tasks-list");
  const tabs = {
    active: document.getElementById("tab-active"),
    completed: document.getElementById("tab-completed"),
    archived: document.getElementById("tab-archived"),
  };
  const counts = state.taskCounts;
  const navCount = document.getElementById("nav-task-count");
  const taskCount = document.getElementById("tasks-count");
  taskViews.forEach((view) => {
    const tab = tabs[view];
    if (!tab) return;
    const selected = state.taskView === view;
    tab.classList.toggle("active", selected);
    tab.setAttribute("aria-selected", String(selected));
  });
  ["active", "completed", "archived"].forEach((view) => {
    const count = document.getElementById(`count-${view}`);
    if (count) count.textContent = String(counts[view] || 0);
  });
  const query = state.taskFilter.trim().toLowerCase();
  const shown = query ? state.tasks.filter((task) => taskMatchesFilter(task, query)) : state.tasks;
  if (navCount) navCount.textContent = String(counts.active + counts.completed);
  if (taskCount) taskCount.textContent = `${shown.length} 项任务`;
  if (!list) return;
  if (!shown.length) {
    const empty = state.taskView === "archived"
      ? "暂无已归档任务"
      : state.taskView === "completed"
        ? "暂无已完成任务"
        : "暂无进行中任务<br /><strong>从记录区转入，或新建一项正式任务</strong>";
    list.innerHTML = `<div class="empty">${query ? "没有匹配的任务" : empty}</div>`;
    return;
  }

  const priorityOptions = (current) => ["P0", "P1", "P2", "P3"]
    .map((priority) => `<option value="${priority}" ${priority === current ? "selected" : ""}>${priority}</option>`)
    .join("");

  list.innerHTML = shown.map((task) => {
    const overdue = isOverdue(task);
    const subtasks = safeArray(task.subtasks);
    const doneCount = subtasks.filter((subtask) => subtask.is_done).length;
    const tags = safeArray(task.tags).slice(0, 4);
    const due = task.due_at ? `<span class="${overdue ? "overdue" : ""}">截止 ${esc(formatDateTime(task.due_at))}${overdue ? " · 已过期" : ""}</span>` : "";
    const planned = task.planned_start ? `<span>计划 ${esc(formatDateTime(task.planned_start))}${task.planned_end ? ` – ${esc(formatTime(task.planned_end))}` : ""}</span>` : "";
    const description = task.description ? `<div class="task-description">${esc(task.description)}</div>` : "";
    const tagLine = tags.length ? `<span class="task-tags">${tags.map((tag) => `#${esc(tag)}`).join(" ")}</span>` : "";
    const progress = Number(task.progress) > 0 ? `<div class="progress" aria-label="进度 ${Math.min(Number(task.progress), 100)}%"><div class="progress-bar" style="width:${Math.min(Math.max(Number(task.progress) || 0, 0), 100)}%"></div></div>` : "";
    const restoreLabel = task.status === "done" ? "恢复到已完成" : "恢复到进行中";
    return `
      <article class="task task-row task-row-${esc(taskViewFor(task))} ${overdue ? "task-overdue" : ""}" data-id="${task.id}" tabindex="0" role="button" aria-label="编辑任务：${esc(task.title)}">
        <div class="task-main">
          <div class="task-head"><div class="title">${esc(task.title)}</div><span class="badge st-${esc(task.status)}">${esc(taskStatusText[task.status] || task.status)}</span></div>
          ${description}
          <div class="row task-meta">
            <label class="priority-field">优先级 <select class="task-priority" data-id="${task.id}" aria-label="修改优先级">${priorityOptions(task.priority)}</select></label>
            ${due}${planned}
            ${task.estimate_minutes ? `<span>预估 ${esc(task.estimate_minutes)} 分钟</span>` : ""}
            ${task.actual_minutes ? `<span>实际 ${esc(task.actual_minutes)} 分钟</span>` : ""}
            ${subtasks.length ? `<span>子任务 ${doneCount}/${subtasks.length}</span>` : ""}
            ${tagLine}
          </div>
          ${progress}
        </div>
        <div class="task-actions">
          ${state.taskView === "archived" ? "" : statusActionButtons(task)}
          ${state.taskView === "archived" ? `<button class="btn small" type="button" data-action="unarchive" data-id="${task.id}">${restoreLabel}</button>` : `<button class="btn small" type="button" data-action="archive" data-id="${task.id}">归档</button>`}
          <button class="btn small danger-ghost" type="button" data-action="delete" data-id="${task.id}">删除</button>
        </div>
      </article>`;
  }).join("");

  list.querySelectorAll(".task").forEach((card) => {
    const open = () => openTaskModal(Number(card.dataset.id));
    card.addEventListener("click", (event) => {
      if (event.target.closest("button, select, input, label")) return;
      open();
    });
    card.addEventListener("keydown", (event) => {
      if ((event.key === "Enter" || event.key === " ") && !event.target.closest("button, select, input, label")) {
        event.preventDefault();
        open();
      }
    });
  });

  list.querySelectorAll(".task-priority").forEach((select) => {
    select.addEventListener("change", async (event) => {
      event.stopPropagation();
      await runBusy(`priority:${select.dataset.id}`, async () => {
        try {
          await invoke("set_task_priority", { id: Number(select.dataset.id), priority: select.value });
          showToast("优先级已更新");
          await refreshAll();
        } catch (error) {
          showError(`更新优先级失败：${errorText(error)}`);
        }
      });
    });
  });

  const statusPatch = { start: "in_progress", done: "done", reopen: "todo" };
  Object.entries(statusPatch).forEach(([action, status]) => {
    list.querySelectorAll(`[data-action="${action}"]`).forEach((button) => {
      button.addEventListener("click", async (event) => {
        event.stopPropagation();
        const task = state.tasks.find((item) => item.id === Number(button.dataset.id));
        if (!task) return;
        await runBusy(`status:${task.id}`, async () => {
          try {
            await invoke("update_task", { task: taskAfterAction(task, action) });
            showToast(`任务已${taskStatusText[status] || status}`);
            await refreshAll();
          } catch (error) {
            showError(`状态更新失败：${errorText(error)}`);
          }
        });
      });
    });
  });

  list.querySelectorAll("[data-action=\"archive\"]").forEach((button) => {
    button.addEventListener("click", async (event) => {
      event.stopPropagation();
      await runBusy(`archive:${button.dataset.id}`, async () => {
        try {
          await invoke("archive_task", { id: Number(button.dataset.id) });
          showToast("任务已归档");
          await refreshAll();
        } catch (error) {
          showError(`归档失败：${errorText(error)}`);
        }
      });
    });
  });

  list.querySelectorAll("[data-action=\"unarchive\"]").forEach((button) => {
    button.addEventListener("click", async (event) => {
      event.stopPropagation();
      await runBusy(`archive:${button.dataset.id}`, async () => {
        try {
          const task = state.tasks.find((item) => item.id === Number(button.dataset.id));
          await invoke("unarchive_task", { id: Number(button.dataset.id) });
          showToast(task?.status === "done" ? "任务已恢复到已完成" : "任务已恢复到进行中");
          await refreshAll();
        } catch (error) {
          showError(`恢复任务失败：${errorText(error)}`);
        }
      });
    });
  });

  list.querySelectorAll("[data-action=\"delete\"]").forEach((button) => {
    button.addEventListener("click", async (event) => {
      event.stopPropagation();
      if (!window.confirm("确定删除该任务？删除后不会出现在任何列表中。")) return;
      await runBusy(`delete-task:${button.dataset.id}`, async () => {
        try {
          await invoke("delete_task", { id: Number(button.dataset.id) });
          showToast("任务已删除");
          await refreshAll();
        } catch (error) {
          showError(`删除任务失败：${errorText(error)}`);
        }
      });
    });
  });
}

function setTaskView(view) {
  if (!taskViews.includes(view)) return;
  state.taskView = view;
  state.tasks = state.taskBuckets[view] || [];
  renderTasks();
  refreshTasks().catch((error) => showError(`任务刷新失败：${errorText(error)}`));
}

/* ===== 记录区检查器与草稿 ===== */
function selectedInboxItem() {
  return state.inbox.find((item) => item.id === state.selectedId) || null;
}

function draftKeyFor(item, index = state.selectedItemIndex) {
  return `${item.id}:${itemKeyFor(item, index) || `ui-item-${index}`}`;
}

function readInboxDraft() {
  return {
    title: document.getElementById("f-title").value,
    kind: document.getElementById("f-kind").value,
    priority: document.getElementById("f-priority").value,
    status: document.getElementById("f-status").value,
    due: document.getElementById("f-due").value,
    estimate: document.getElementById("f-estimate").value,
    start: document.getElementById("f-start").value,
    end: document.getElementById("f-end").value,
    description: document.getElementById("f-description").value,
    tags: document.getElementById("f-tags").value,
    subtasks: document.getElementById("f-subtasks").value,
  };
}

function applyInboxDraft(values) {
  document.getElementById("f-title").value = values.title || "";
  document.getElementById("f-kind").value = values.kind || "task";
  document.getElementById("f-priority").value = values.priority || "P2";
  document.getElementById("f-status").value = values.status || "todo";
  document.getElementById("f-due").value = values.due || "";
  document.getElementById("f-estimate").value = values.estimate ?? "";
  document.getElementById("f-start").value = values.start || "";
  document.getElementById("f-end").value = values.end || "";
  document.getElementById("f-description").value = values.description || "";
  document.getElementById("f-tags").value = values.tags || "";
  document.getElementById("f-subtasks").value = values.subtasks || "";
}

function saveCurrentInboxDraft() {
  const item = selectedInboxItem();
  if (!item || !document.getElementById("detail-panel") || document.getElementById("detail-panel").classList.contains("hidden")) return;
  const key = draftKeyFor(item);
  const existing = state.inboxDrafts.get(key);
  if (existing?.dirty) existing.values = readInboxDraft();
}

function markInboxDraftDirty() {
  const item = selectedInboxItem();
  if (!item) return;
  state.inboxDrafts.set(draftKeyFor(item), { dirty: true, values: readInboxDraft() });
}

function fillDraftFromItem(ai, item, index) {
  const saved = state.inboxDrafts.get(draftKeyFor(item, index));
  if (saved?.dirty) {
    applyInboxDraft(saved.values);
    return;
  }
  applyInboxDraft({
    title: ai?.title || item.raw_text?.slice(0, 40) || (item.image_path ? "图片任务" : ""),
    kind: ai?.kind || "task",
    priority: ai?.priority || "P2",
    status: "todo",
    due: fmtDtLocal(ai?.deadline),
    estimate: ai?.estimate_minutes ?? "",
    start: "",
    end: "",
    description: ai?.description || item.raw_text || "",
    tags: safeArray(ai?.tags).join(", "),
    subtasks: safeArray(ai?.subtasks).join("\n"),
  });
}

function updatePromotionUi(item, index) {
  const button = document.getElementById("btn-promote");
  const note = document.getElementById("f-item-state");
  const key = itemKeyFor(item, index);
  const promoted = isItemPromoted(item, index);
  const { promoted: promotedCount, total } = promotionCounts(item);
  const progress = document.getElementById("promotion-progress");
  if (progress) progress.textContent = total > 1 ? `已转为正式任务 ${promotedCount}/${total}` : promoted ? "已转为正式任务" : "待转为正式任务";
  if (button) {
    const operationKey = key ? `promote:${item.id}:${key}` : null;
    const busyKey = button.dataset.busyKey;
    const busyForCurrentItem = !!busyKey && busyKey === operationKey && state.busy.has(busyKey);
    if (busyForCurrentItem) {
      button.disabled = true;
      button.setAttribute("aria-busy", "true");
      button.textContent = "转换中…";
    } else {
      // The button is shared by all records. If another record's request owns
      // it, release that visual ownership before rendering the current record;
      // the old request's finally block will then fail its owner check.
      if (busyKey) {
        delete button.dataset.busyKey;
        delete button.dataset.idleLabel;
      }
      button.removeAttribute("aria-busy");
      button.disabled = promoted || !key;
      button.textContent = promoted ? "已转为正式任务" : "转为正式任务";
    }
  }
  if (note) {
    note.classList.toggle("is-promoted", promoted);
    note.textContent = promoted
      ? "该待办已转为正式任务，可在正式任务页继续编辑。"
      : key ? "此项尚未转为正式任务。"
        : "正在等待后端生成该图片待办的稳定标识，请稍后刷新。";
  }
}

function renderAiSummary(item, ai, items) {
  const box = document.getElementById("ai-summary");
  if (!box) return;
  const status = item.ai_status;
  if (status === "failed") {
    box.textContent = `解析失败：${item.ai_result || "未知错误"}\n可以点“重新解析”重试。`;
    return;
  }
  if (status === "unconfigured") {
    box.textContent = "尚未配置 API Key。打开设置填写后，再重新解析这条记录。";
    return;
  }
  if (["queued", "pending", "processing"].includes(status)) {
    box.textContent = status === "processing" ? "AI 正在整理这条记录，请稍候。" : "这条记录已排队，等待 AI 整理。";
    return;
  }
  if (status === "skipped") {
    box.textContent = "这条记录跳过了自动解析，可以在设置中开启后重新解析。";
    return;
  }
  if (!ai) {
    box.textContent = status === "partial" ? "结果待核对：解析输出不完整，请编辑草稿后再转为正式任务。" : "暂无可展示的 AI 解析结果。";
    return;
  }
  const lines = [];
  if (status === "partial") lines.push("结果待核对：这次解析只恢复了部分输出，请检查下面的草稿。");
  if (items.length > 1 || hasAiItemList(item)) {
    lines.push(`图片中识别出 ${items.length} 个待办，可在下方切换并逐项转为正式任务。`);
    if (ai.summary) lines.push(`图片概述：${ai.summary}`);
  } else {
    lines.push(`类型：${ai.kind || "-"}　优先级：${ai.priority || "-"}　把握：${ai.confidence ?? "-"}`);
    if (ai.summary) lines.push(`总结：${ai.summary}`);
    if (ai.deadline) lines.push(`截止：${formatDateTime(ai.deadline)}`);
    if (ai.estimate_minutes) lines.push(`预估：${ai.estimate_minutes} 分钟`);
    if (safeArray(ai.tags).length) lines.push(`标签：${ai.tags.join("、")}`);
    if (safeArray(ai.subtasks).length) lines.push(`子任务：${ai.subtasks.join("；")}`);
    if (safeArray(ai.missing_info).length) lines.push(`待补充：${ai.missing_info.join("；")}`);
  }
  box.textContent = lines.join("\n");
}

async function renderDetail(id) {
  const item = state.inbox.find((entry) => entry.id === id);
  if (!item) return;
  const token = ++state.detailRenderToken;
  const panel = document.getElementById("detail-panel");
  panel.classList.remove("hidden");
  panel.setAttribute("aria-hidden", "false");
  document.getElementById("detail-empty").classList.add("hidden");
  document.getElementById("detail-content").classList.remove("hidden");
  document.getElementById("detail-source").textContent = `${sourceText(item.source)} · ${formatDateTime(item.created_at)}${item.image_path ? " · 图片附件" : ""}`;
  const status = document.getElementById("ai-status");
  status.textContent = statusText[item.ai_status] || item.ai_status || "未知状态";
  status.className = `badge ${item.ai_status || "pending"}`;
  document.getElementById("ai-model").textContent = item.ai_model ? `模型：${item.ai_model}` : "";
  document.getElementById("raw-text").textContent = item.raw_text || (item.image_path ? "（无文字说明，仅图片）" : "");

  const imageBlock = document.getElementById("detail-image");
  const image = document.getElementById("detail-img");
  if (item.image_path) {
    imageBlock.classList.remove("hidden");
    image.alt = "记录附件";
    try {
      image.src = await invoke("read_inbox_image", { id: item.id });
    } catch {
      if (token === state.detailRenderToken) imageBlock.classList.add("hidden");
    }
  } else {
    imageBlock.classList.add("hidden");
    image.src = "";
  }
  if (token !== state.detailRenderToken || state.selectedId !== id) return;

  const ai = parseAiResult(item.ai_result);
  const items = aiItemsFor(item);
  renderAiSummary(item, ai, items);
  const selectorWrap = document.getElementById("f-item-wrap");
  const selector = document.getElementById("f-item");
  if (hasAiItemList(item)) {
    selectorWrap.classList.remove("hidden");
    selector.innerHTML = items.map((entry, index) => {
      const added = isItemPromoted(item, index);
      return `<option value="${index}" ${index === state.selectedItemIndex ? "selected" : ""}>${added ? "[已添加] " : ""}${esc(entry.title || `待办 ${index + 1}`)}</option>`;
    }).join("");
    const validIndex = Math.min(Math.max(state.selectedItemIndex, 0), Math.max(items.length - 1, 0));
    state.selectedItemIndex = validIndex;
    selector.value = String(validIndex);
    selector.onchange = () => {
      saveCurrentInboxDraft();
      state.selectedItemIndex = Number(selector.value);
      fillDraftFromItem(items[state.selectedItemIndex], item, state.selectedItemIndex);
      updatePromotionUi(item, state.selectedItemIndex);
    };
  } else {
    selectorWrap.classList.add("hidden");
    selector.onchange = null;
    state.selectedItemIndex = 0;
  }
  const currentAi = items[state.selectedItemIndex] || ai;
  fillDraftFromItem(currentAi, item, state.selectedItemIndex);
  updatePromotionUi(item, state.selectedItemIndex);
}

function selectInbox(id) {
  if (state.selectedId === id) {
    document.getElementById("detail-panel")?.classList.remove("hidden");
    return;
  }
  saveCurrentInboxDraft();
  state.selectedId = id;
  state.selectedItemIndex = 0;
  renderInbox();
  renderDetail(id);
}

function closeInspector() {
  saveCurrentInboxDraft();
  state.selectedId = null;
  state.selectedItemIndex = 0;
  state.detailRenderToken += 1;
  const panel = document.getElementById("detail-panel");
  panel.setAttribute("aria-hidden", "true");
  document.getElementById("detail-empty").classList.remove("hidden");
  document.getElementById("detail-content").classList.add("hidden");
  renderInbox();
}

async function promoteSelected() {
  const item = selectedInboxItem();
  if (!item) return;
  const index = state.selectedItemIndex;
  const itemKey = itemKeyFor(item, index);
  if (isItemPromoted(item, index)) {
    showToast("该待办已经转为正式任务");
    return;
  }
  if (!itemKey) {
    showError("该图片待办还没有稳定标识，请稍后刷新再试");
    return;
  }
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
    tags: document.getElementById("f-tags").value.split(/[,，]/).map((tag) => tag.trim()).filter(Boolean),
    subtasks: document.getElementById("f-subtasks").value.split("\n").map((text) => text.trim()).filter(Boolean),
  };
  const operationKey = `promote:${item.id}:${itemKey}`;
  await runBusy(operationKey, async () => {
    try {
      await invoke("promote_inbox", { inboxId: item.id, draft, itemKey });
      state.inboxDrafts.delete(draftKeyFor(item, index));
      const local = state.inbox.find((entry) => entry.id === item.id);
      if (local) {
        if (!Array.isArray(local.promoted_keys)) local.promoted_keys = [];
        if (!local.promoted_keys.includes(itemKey)) local.promoted_keys.push(itemKey);
      }
      showToast("已转为正式任务");
      await refreshAll();
    } catch (error) {
      showError(`转为正式任务失败：${errorText(error)}`);
    }
  }, "btn-promote", "转换中…", () => {
    const current = selectedInboxItem();
    if (current) updatePromotionUi(current, state.selectedItemIndex);
  });
}

/* ===== 任务编辑弹窗 ===== */
function getEditingTask() {
  const loadedTasks = taskViews.flatMap((view) => state.taskBuckets[view] || []);
  return loadedTasks.find((task) => task.id === state.editingId)
    || state.tasks.find((task) => task.id === state.editingId)
    || null;
}

function openModal(id, preferredFocusId) {
  const modal = document.getElementById(id);
  if (!modal) return;
  state.modalId = id;
  state.modalLastFocus = document.activeElement;
  modal.classList.remove("hidden");
  modal.setAttribute("aria-hidden", "false");
  setTimeout(() => {
    const target = preferredFocusId ? document.getElementById(preferredFocusId) : modal.querySelector("input, select, textarea, button");
    target?.focus();
  }, 0);
}

function closeModal(id = state.modalId) {
  const modal = document.getElementById(id);
  if (!modal) return;
  modal.classList.add("hidden");
  modal.setAttribute("aria-hidden", "true");
  if (state.modalId === id) {
    state.modalId = null;
    const target = state.modalLastFocus;
    state.modalLastFocus = null;
    if (target && typeof target.focus === "function") target.focus();
  }
}

function setTaskModalMode(mode) {
  const create = mode === "create";
  document.getElementById("e-modal-title").textContent = create ? "新建任务" : "编辑任务";
  document.getElementById("e-subtasks-zone").classList.toggle("hidden", create);
  document.getElementById("e-subtasks-new").classList.toggle("hidden", !create);
  document.getElementById("e-actual-field").classList.toggle("hidden", create);
  document.getElementById("e-progress-field").classList.toggle("hidden", create);
}

function openTaskModal(id) {
  const task = state.tasks.find((entry) => entry.id === id);
  if (!task) return;
  state.editingId = id;
  setTaskModalMode("edit");
  document.getElementById("e-meta").textContent = `创建于 ${formatDateTime(task.created_at)} · 来源 ${task.source_inbox_id ? `记录区 #${task.source_inbox_id}` : "手动创建"}`;
  document.getElementById("e-title").value = task.title || "";
  document.getElementById("e-kind").value = task.kind || "task";
  document.getElementById("e-status").value = task.status || "todo";
  document.getElementById("e-priority").value = task.priority || "P2";
  document.getElementById("e-due").value = fmtDtLocal(task.due_at);
  document.getElementById("e-estimate").value = task.estimate_minutes ?? "";
  document.getElementById("e-actual").value = task.actual_minutes ?? 0;
  document.getElementById("e-progress").value = task.progress ?? 0;
  document.getElementById("e-start").value = fmtDtLocal(task.planned_start);
  document.getElementById("e-end").value = fmtDtLocal(task.planned_end);
  document.getElementById("e-description").value = task.description || "";
  document.getElementById("e-tags").value = safeArray(task.tags).join(", ");
  renderEditSubtasks();
  openModal("task-modal", "e-title");
}

function closeTaskModal() {
  state.editingId = null;
  closeModal("task-modal");
}

function openTaskModalForCreate() {
  state.editingId = null;
  setTaskModalMode("create");
  document.getElementById("e-meta").textContent = "直接创建正式任务，不经过记录区";
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
  openModal("task-modal", "e-title");
}

function renderEditSubtasks() {
  const task = getEditingTask();
  const box = document.getElementById("e-subtasks");
  if (!box) return;
  const subtasks = safeArray(task?.subtasks);
  if (!task || !subtasks.length) {
    box.innerHTML = '<div class="subtask-empty">暂无子任务</div>';
    return;
  }
  box.innerHTML = subtasks.map((subtask) => `
    <div class="subtask ${subtask.is_done ? "is-done" : ""}" data-id="${subtask.id}">
      <label class="subtask-check"><input type="checkbox" ${subtask.is_done ? "checked" : ""} /><span class="subtask-text">${esc(subtask.text)}</span></label>
      <button class="btn small danger-ghost subtask-del" type="button" data-id="${subtask.id}">删除</button>
    </div>`).join("");
  box.querySelectorAll("input[type=checkbox]").forEach((checkbox) => {
    checkbox.addEventListener("change", async () => {
      const subtaskId = Number(checkbox.closest(".subtask").dataset.id);
      const desired = checkbox.checked;
      await runBusy(`subtask-done:${subtaskId}`, async () => {
        try {
          await invoke("set_subtask_done", { id: subtaskId, isDone: desired });
          await refreshAll();
        } catch (error) {
          checkbox.checked = !desired;
          showError(`更新子任务失败：${errorText(error)}`);
        }
      });
    });
  });
  box.querySelectorAll(".subtask-del").forEach((button) => {
    button.addEventListener("click", async () => {
      const subtaskId = Number(button.dataset.id);
      await runBusy(`subtask-delete:${subtaskId}`, async () => {
        try {
          await invoke("delete_subtask", { id: subtaskId });
          await refreshAll();
        } catch (error) {
          showError(`删除子任务失败：${errorText(error)}`);
        }
      });
    });
  });
}

async function addSubtaskFromInput() {
  const input = document.getElementById("e-subtask-new");
  const text = input.value.trim();
  if (!text || !state.editingId) return;
  await runBusy("add-subtask", async () => {
    try {
      await invoke("add_subtask", { taskId: state.editingId, text });
      input.value = "";
      await refreshAll();
    } catch (error) {
      showError(`添加子任务失败：${errorText(error)}`);
    }
  }, "e-subtask-add", "添加中…");
}

function readTaskDraft() {
  const number = (id, fallback) => {
    const value = document.getElementById(id).value;
    if (value === "") return fallback;
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : fallback;
  };
  return {
    title: document.getElementById("e-title").value.trim(),
    description: document.getElementById("e-description").value.trim(),
    kind: document.getElementById("e-kind").value,
    status: document.getElementById("e-status").value,
    priority: document.getElementById("e-priority").value,
    due_at: dtToIso(document.getElementById("e-due").value),
    estimate_minutes: number("e-estimate", null),
    planned_start: dtToIso(document.getElementById("e-start").value),
    planned_end: dtToIso(document.getElementById("e-end").value),
    tags: document.getElementById("e-tags").value.split(/[,，]/).map((tag) => tag.trim()).filter(Boolean),
  };
}

async function saveTaskModal() {
  const draft = readTaskDraft();
  if (!draft.title) {
    showError("任务标题不能为空");
    document.getElementById("e-title").focus();
    return;
  }
  await runBusy("save-task", async () => {
    try {
      if (!state.editingId) {
        const subtasks = document.getElementById("e-subtasks-new").value.split("\n").map((text) => text.trim()).filter(Boolean);
        await invoke("create_task", { draft: { ...draft, subtasks } });
        state.taskView = "active";
        closeTaskModal();
        switchView("tasks");
        showToast("任务已创建");
        await refreshAll();
        return;
      }
      const task = getEditingTask();
      if (!task) {
        closeTaskModal();
        return;
      }
      const actualValue = document.getElementById("e-actual").value;
      const progressValue = document.getElementById("e-progress").value;
      await invoke("update_task", {
        task: {
          ...task,
          ...draft,
          actual_minutes: actualValue === "" ? 0 : Math.max(0, Number(actualValue) || 0),
          progress: Math.max(0, Math.min(100, Number(progressValue) || 0)),
        },
      });
      closeTaskModal();
      showToast("任务已保存");
      await refreshAll();
    } catch (error) {
      showError(`保存失败：${errorText(error)}`);
    }
  }, "btn-save-task", "保存中…");
}

/* ===== 设置 ===== */
const PRESETS = {
  deepseek: { provider: "openai", base_url: "https://api.deepseek.com", model: "deepseek-v4-flash" },
  openai: { provider: "openai", base_url: "https://api.openai.com/v1", model: "gpt-4o-mini" },
  moonshot: { provider: "openai", base_url: "https://api.moonshot.cn/v1", model: "kimi-latest" },
  qwen: { provider: "openai", base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "qwen-plus" },
  glm: { provider: "openai", base_url: "https://open.bigmodel.cn/api/paas/v4", model: "glm-4-flash" },
  ollama: { provider: "openai", base_url: "http://localhost:11434/v1", model: "qwen3:8b" },
  anthropic: { provider: "anthropic", base_url: "https://api.anthropic.com", model: "claude-sonnet-4-5" },
};

async function loadSettings() {
  currentSettings = await invoke("get_settings");
  const settings = currentSettings || {};
  document.getElementById("s-provider").value = settings.provider || "openai";
  // The backend deliberately never returns api_key. Always clear the secret field.
  document.getElementById("s-api-key").value = "";
  document.getElementById("s-remove-key").checked = false;
  document.getElementById("s-key-state").textContent = settings.has_api_key ? "已保存 API Key" : "未保存 API Key";
  document.getElementById("s-base-url").value = settings.base_url || "";
  document.getElementById("s-model").value = settings.model || "";
  document.getElementById("s-vision-model").value = settings.vision_model || "";
  document.getElementById("s-disable-reasoning").checked = settings.disable_reasoning !== false;
  document.getElementById("s-auto-parse").checked = !!settings.auto_parse;
  document.getElementById("s-autostart").checked = !!settings.autostart;
}

async function openSettings() {
  try {
    await loadSettings();
    openModal("settings-modal", "s-provider");
  } catch (error) {
    showError(`读取设置失败：${errorText(error)}`);
  }
}

async function saveSettings() {
  await runBusy("save-settings", async () => {
    const newKey = document.getElementById("s-api-key").value.trim();
    const removeKey = document.getElementById("s-remove-key").checked;
    const settings = {
      api_key: newKey,
      remove_api_key: removeKey,
      provider: document.getElementById("s-provider").value,
      base_url: document.getElementById("s-base-url").value.trim() || "https://api.deepseek.com",
      model: document.getElementById("s-model").value.trim() || "deepseek-v4-flash",
      vision_model: document.getElementById("s-vision-model").value.trim() || null,
      disable_reasoning: document.getElementById("s-disable-reasoning").checked,
      auto_parse: document.getElementById("s-auto-parse").checked,
      autostart: document.getElementById("s-autostart").checked,
      quick_hotkey: currentSettings?.quick_hotkey || "Ctrl+Alt+Q",
      main_hotkey: currentSettings?.main_hotkey || "Ctrl+Alt+W",
    };
    try {
      await invoke("save_settings", { settings });
      currentSettings = {
        provider: settings.provider,
        base_url: settings.base_url,
        model: settings.model,
        vision_model: settings.vision_model,
        disable_reasoning: settings.disable_reasoning,
        auto_parse: settings.auto_parse,
        autostart: settings.autostart,
        quick_hotkey: settings.quick_hotkey,
        main_hotkey: settings.main_hotkey,
        has_api_key: removeKey ? false : Boolean(newKey) || !!currentSettings?.has_api_key,
      };
      document.getElementById("s-api-key").value = "";
      document.getElementById("s-remove-key").checked = false;
      document.getElementById("s-key-state").textContent = currentSettings.has_api_key ? "已保存 API Key" : "未保存 API Key";
      closeModal("settings-modal");
      showToast("设置已保存");
    } catch (error) {
      showError(`保存设置失败：${errorText(error)}`);
    }
  }, "btn-save-settings", "保存中…");
}

/* ===== 刷新与事件 ===== */
async function refreshInbox() {
  const result = await invoke("list_inbox");
  state.inbox = Array.isArray(result) ? result : [];
  renderInbox();
}

async function refreshTasks() {
  const refreshToken = ++state.taskRefreshToken;
  try {
    const [unarchivedResult, archivedResult] = await Promise.all([
      invoke("list_tasks"),
      invoke("list_archived_tasks"),
    ]);
    if (refreshToken !== state.taskRefreshToken) return false;
    const buckets = classifyTasks(mergeTaskSources(unarchivedResult, archivedResult));
    state.taskBuckets = buckets;
    state.taskCounts = {
      active: buckets.active.length,
      completed: buckets.completed.length,
      archived: buckets.archived.length,
    };
    state.tasks = buckets[state.taskView] || [];
    renderTasks();
    return true;
  } catch (error) {
    if (refreshToken !== state.taskRefreshToken) return false;
    throw error;
  }
}

function inboxChanged(before, after) {
  return !before || !after
    || before.ai_status !== after.ai_status
    || before.ai_result !== after.ai_result
    || before.ai_generation !== after.ai_generation
    || before.is_promoted !== after.is_promoted
    || JSON.stringify(before.item_keys || []) !== JSON.stringify(after.item_keys || [])
    || JSON.stringify(before.promoted_keys || []) !== JSON.stringify(after.promoted_keys || []);
}

async function refreshAll() {
  if (state.refreshPromise) return state.refreshPromise;
  const previousInbox = state.inbox;
  state.refreshPromise = (async () => {
    await Promise.all([refreshInbox(), refreshTasks()]);
    if (state.selectedId) {
      const current = state.inbox.find((item) => item.id === state.selectedId);
      const previous = previousInbox.find((item) => item.id === state.selectedId);
      if (!current) closeInspector();
      else if (inboxChanged(previous, current)) await renderDetail(state.selectedId);
    }
    if (state.editingId) renderEditSubtasks();
  })();
  try {
    await state.refreshPromise;
  } finally {
    state.refreshPromise = null;
  }
}

async function subscribe(eventName, handler) {
  if (!listen) return;
  try {
    await listen(eventName, handler);
  } catch (error) {
    // A missing optional event listener must not stop the main window loading.
    console.warn(`无法监听 ${eventName}`, error);
  }
}

function handleKeydown(event) {
  if (!state.modalId) return;
  const modal = document.getElementById(state.modalId);
  if (!modal || modal.classList.contains("hidden")) return;
  if (event.key === "Escape") {
    event.preventDefault();
    if (state.modalId === "task-modal") closeTaskModal();
    else closeModal(state.modalId);
    return;
  }
  if (event.key !== "Tab") return;
  const focusable = [...modal.querySelectorAll("button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex=\"-1\"])")];
  if (!focusable.length) return;
  const first = focusable[0];
  const last = focusable[focusable.length - 1];
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first.focus();
  }
}

function attachModalEvents() {
  document.querySelectorAll("[data-modal-close]").forEach((button) => {
    button.addEventListener("click", () => {
      if (button.dataset.modalClose === "task-modal") closeTaskModal();
      else closeModal(button.dataset.modalClose);
    });
  });
  document.querySelectorAll(".modal").forEach((modal) => {
    modal.addEventListener("click", (event) => {
      if (event.target !== modal) return;
      if (modal.id === "task-modal") closeTaskModal();
      else closeModal(modal.id);
    });
  });
}

async function init() {
  document.querySelectorAll(".nav-item[data-view]").forEach((button) => {
    button.addEventListener("click", () => switchView(button.dataset.view));
  });
  document.getElementById("btn-refresh").addEventListener("click", () => refreshAll().catch((error) => showError(`刷新失败：${errorText(error)}`)));
  document.getElementById("btn-process-pending").addEventListener("click", async () => {
    await runBusy("process-pending", async () => {
      try {
        const count = await invoke("process_pending_inbox");
        showToast(count ? `已启动 ${count} 条记录的 AI 解析` : "没有待处理记录");
        await refreshAll();
      } catch (error) {
        showError(`启动解析失败：${errorText(error)}`);
      }
    }, "btn-process-pending", "启动中…");
  });
  document.getElementById("btn-settings").addEventListener("click", openSettings);
  document.getElementById("btn-close-inspector").addEventListener("click", closeInspector);
  document.getElementById("tab-active").addEventListener("click", () => setTaskView("active"));
  document.getElementById("tab-completed")?.addEventListener("click", () => setTaskView("completed"));
  document.getElementById("tab-archived").addEventListener("click", () => setTaskView("archived"));
  document.getElementById("btn-new-task").addEventListener("click", openTaskModalForCreate);
  document.getElementById("task-filter").addEventListener("input", (event) => {
    state.taskFilter = event.target.value;
    renderTasks();
  });
  document.getElementById("btn-promote").addEventListener("click", promoteSelected);
  document.getElementById("btn-delete-inbox").addEventListener("click", async () => {
    const item = selectedInboxItem();
    if (!item || !window.confirm("只会在记录区中忽略这条记录，不会删除正式任务中的关联记录。继续？")) return;
    await runBusy(`delete-inbox:${item.id}`, async () => {
      try {
        await invoke("delete_inbox", { id: item.id });
        state.inboxDrafts.delete(draftKeyFor(item));
        closeInspector();
        showToast("记录已忽略");
        await refreshAll();
      } catch (error) {
        showError(`忽略记录失败：${errorText(error)}`);
      }
    }, "btn-delete-inbox", "处理中…");
  });
  document.getElementById("btn-reparse").addEventListener("click", async () => {
    const item = selectedInboxItem();
    if (!item) return;
    await runBusy(`reparse:${item.id}`, async () => {
      try {
        await invoke("parse_inbox_item", { id: item.id });
        showToast("已重新加入解析队列");
        await refreshAll();
      } catch (error) {
        showError(`重新解析失败：${errorText(error)}`);
      }
    }, "btn-reparse", "排队中…");
  });
  document.getElementById("btn-close-settings").addEventListener("click", () => closeModal("settings-modal"));
  document.getElementById("btn-save-settings").addEventListener("click", saveSettings);
  document.getElementById("s-preset").addEventListener("change", (event) => {
    const preset = PRESETS[event.target.value];
    if (!preset) return;
    document.getElementById("s-provider").value = preset.provider;
    document.getElementById("s-base-url").value = preset.base_url;
    document.getElementById("s-model").value = preset.model;
  });
  document.getElementById("btn-close-task").addEventListener("click", closeTaskModal);
  document.getElementById("btn-save-task").addEventListener("click", saveTaskModal);
  document.getElementById("e-subtask-add").addEventListener("click", addSubtaskFromInput);
  document.getElementById("e-subtask-new").addEventListener("keydown", (event) => {
    if (event.isComposing || event.keyCode === 229) return;
    if (event.key === "Enter") {
      event.preventDefault();
      addSubtaskFromInput();
    }
  });
  ["f-title", "f-kind", "f-priority", "f-status", "f-due", "f-estimate", "f-start", "f-end", "f-description", "f-tags", "f-subtasks"].forEach((id) => {
    document.getElementById(id).addEventListener("input", markInboxDraftDirty);
    document.getElementById(id).addEventListener("change", markInboxDraftDirty);
  });
  attachModalEvents();
  document.addEventListener("keydown", handleKeydown);
  await subscribe("inbox-updated", () => refreshAll().catch((error) => showError(`记录区刷新失败：${errorText(error)}`)));
  await subscribe("tasks-updated", () => refreshAll().catch((error) => showError(`任务刷新失败：${errorText(error)}`)));
  await subscribe("operation-error", (event) => {
    const payload = event?.payload || {};
    showError(`${payload.id ? `${payload.id}：` : ""}${payload.message || "操作失败"}`);
  });
  window.addEventListener("focus", () => refreshAll().catch((error) => showError(`刷新失败：${errorText(error)}`)));
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) refreshAll().catch((error) => showError(`刷新失败：${errorText(error)}`));
  });
  setInterval(() => {
    if (!document.hidden) refreshAll().catch((error) => showError(`刷新失败：${errorText(error)}`));
  }, 4000);
  switchView("inbox");
  await refreshAll();
}

if (typeof document !== "undefined") {
  document.addEventListener("DOMContentLoaded", () => init().catch((error) => showError(`工作台加载失败：${errorText(error)}`)));
}

if (typeof module !== "undefined" && module.exports) {
  module.exports = {
    localDateParts,
    formatDateTime,
    formatDateOnly,
    formatTime,
    fmtDtLocal,
    dtToIso,
    classifyTasks,
    isTaskArchived,
    mergeTaskSources,
    taskAfterAction,
    taskViewFor,
  };
}
