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
  inboxFilter: "all",
  inboxSearch: "",
  tasks: [],
  taskBuckets: { active: [], completed: [], archived: [] },
  taskCounts: { active: 0, completed: 0, archived: 0 },
  taskRefreshToken: 0,
  selectedId: null,
  selectedItemIndex: 0,
  taskView: "active",
  taskFilter: "",
  taskQuickFilter: "all",
  editingId: null,
  view: "inbox",
  inboxDrafts: new Map(),
  checkedDraftSteps: new Map(),
  busy: new Set(),
  refreshPromise: null,
  detailRenderToken: 0,
  lastInspectorId: null,
  modalId: null,
  modalLastFocus: null,
  animateInboxList: true,
  animateTaskList: true,
  knownInboxIds: new Set(),
  knownTaskIds: new Set(),
  lastInboxItemsSig: "",
  lastTaskRenderSig: "",
  didAutoSelectInitial: false,
};

const statusText = {
  queued: "排队中",
  pending: "待解析",
  processing: "解析中",
  done: "待确认",
  partial: "待核对",
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

const kindText = {
  task: "任务",
  requirement: "需求",
  idea: "想法",
  note: "记录",
};

const personaText = {
  auto: "🤖 智能路由",
  dev: "💻 研发专家",
  pm: "📋 产品项目",
  gtd: "🎯 GTD 行动",
};

const priorityFriendlyText = {
  P0: "紧急优先级",
  P1: "高优先级",
  P2: "中优先级",
  P3: "低优先级",
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
    kicker: "INBOX STUDIO",
    title: "收集箱",
    subtitle: "随手收集一句话或截图，AI 自动提炼结构化草稿，由你确认入库。",
  },
  tasks: {
    kicker: "TASK BOARD",
    title: "任务",
    subtitle: "把确认过的事项放入看板，轻快推进直至达成。",
  },
};

function applyTheme(theme) {
  const next = theme === "dark" ? "dark" : "light";
  if (typeof document !== "undefined") {
    document.documentElement.setAttribute("data-theme", next);
    const label = document.getElementById("theme-toggle-label");
    if (label) label.textContent = next === "dark" ? "🌙 极光暗夜" : "☀️ 活力日光";
  }
  try {
    if (typeof window !== "undefined" && window.localStorage) {
      window.localStorage.setItem("taskvault-theme", next);
    }
  } catch {
    // Ignore storage errors in restricted environments.
  }
}

function initTheme() {
  let saved = "light";
  try {
    if (typeof window !== "undefined" && window.localStorage) {
      saved = window.localStorage.getItem("taskvault-theme") || "light";
    }
  } catch {
    saved = "light";
  }
  applyTheme(saved);
}

let toastTimer = null;
let toastHideTimer = null;
let inspectorCloseTimer = null;
const modalCloseTimers = new Map();
let currentSettings = null;

function replayAnimation(el, className) {
  if (!el) return;
  el.classList.remove(className);
  void el.offsetWidth;
  el.classList.add(className);
  el.addEventListener("animationend", () => {
    el.classList.remove(className);
  }, { once: true });
}

function setTextWithPop(id, nextText) {
  const el = document.getElementById(id);
  if (!el) return;
  const prev = el.textContent;
  el.textContent = nextText;
  if (prev !== "" && prev !== nextText) {
    replayAnimation(el, "value-pop");
  }
}

function animateCardAction(card, action, durationMs = 175) {
  if (!card) return Promise.resolve();
  card.classList.add(`card-acting-${action}`);
  return new Promise((resolve) => setTimeout(resolve, durationMs));
}

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

function getRelativeDateHeading(value) {
  const date = asDate(value);
  if (!date) return "更早";
  const now = new Date();
  const todayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const itemStart = new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
  const diffDays = Math.round((itemStart - todayStart) / 86400000);
  if (diffDays === 0) return "今天";
  if (diffDays === -1) return "昨天";
  if (diffDays === -2) return "前天";
  if (date.getFullYear() === now.getFullYear()) {
    return `${date.getMonth() + 1}月${date.getDate()}日`;
  }
  return `${date.getFullYear()}年${date.getMonth() + 1}月${date.getDate()}日`;
}

function formatFriendlyDeadline(value) {
  const date = asDate(value);
  if (!date) {
    return { main: "未设截止", sub: "（点击设置）" };
  }
  const now = new Date();
  const todayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const targetStart = new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
  const diffDays = Math.round((targetStart - todayStart) / 86400000);
  const hh = String(date.getHours()).padStart(2, "0");
  const mm = String(date.getMinutes()).padStart(2, "0");
  const timeStr = `${hh}:${mm}`;
  const weekdays = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];
  const month = date.getMonth() + 1;
  const day = date.getDate();
  const weekday = weekdays[date.getDay()];

  let main = `${month}月${day}日 ${timeStr}`;
  if (diffDays === 0) main = `今天 ${timeStr}`;
  else if (diffDays === 1) main = `明天 ${timeStr}`;
  else if (diffDays === 2) main = `后天 ${timeStr}`;
  else if (diffDays === -1) main = `昨天 ${timeStr}`;

  return {
    main,
    sub: `（${month}月${day}日 ${weekday}）`,
  };
}

function cnItemCountWord(n) {
  if (n === 2) return "两";
  const map = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九", "十"];
  return map[n] || String(n);
}

function splitInboxCardCopy(item) {
  const ai = parseAiResult(item?.ai_result);
  const items = aiItemsFor(item);
  const raw = String(item?.raw_text || "").trim();

  if (raw) {
    const match = raw.match(/^([^，。：:；;\n]{2,26})[，。：:；;\n]+\s*(.+)$/s);
    if (match) {
      return {
        title: match[1].trim(),
        subtitle: match[2].replace(/\s+/g, " ").trim(),
      };
    }
    let subtitle = "";
    if (hasAiItemList(item) && items.length > 1) {
      subtitle = `图片中有${cnItemCountWord(items.length)}个待办事项`;
    } else if (ai?.summary && ai.summary.trim() !== raw) {
      subtitle = ai.summary.trim();
    } else if (ai?.description && ai.description.trim() !== raw) {
      subtitle = ai.description.trim();
    } else if (item?.image_path) {
      subtitle = "包含图片附件";
    } else {
      subtitle = statusText[item?.ai_status] || "待确认";
    }
    return { title: raw, subtitle };
  }

  if (hasAiItemList(item) && items.length > 1) {
    return {
      title: items[0]?.title || "截图待办记录",
      subtitle: `图片中有${cnItemCountWord(items.length)}个待办事项`,
    };
  }

  return {
    title: ai?.title || (item?.image_path ? "图片附件记录" : "无文字内容"),
    subtitle: ai?.summary || ai?.description || (item?.image_path ? "包含图片附件" : "待处理记录"),
  };
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
  clearTimeout(toastTimer);
  clearTimeout(toastHideTimer);
  el.textContent = message;
  el.classList.toggle("error", kind === "error");
  el.classList.remove("hidden", "toast-leaving");
  void el.offsetWidth;
  el.classList.add("toast-visible");
  toastTimer = setTimeout(() => {
    el.classList.remove("toast-visible");
    el.classList.add("toast-leaving");
    toastHideTimer = setTimeout(() => {
      el.classList.add("hidden");
      el.classList.remove("toast-leaving");
    }, 190);
  }, kind === "error" ? 4200 : 2500);
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

function updateWorkspacePulse() {
  const unpromotedCount = state.inbox.filter((item) => {
    const { promoted, total } = promotionCounts(item);
    return promoted < total;
  }).length;
  const activeCount = state.taskCounts.active || 0;
  const doneCount = (state.taskCounts.completed || 0)
    + safeArray(state.taskBuckets.archived).filter((t) => t.status === "done").length;
  const totalTasks = activeCount + doneCount;
  const rate = totalTasks > 0 ? `完成 ${Math.round((doneCount / totalTasks) * 100)}%` : "就绪";

  setTextWithPop("pulse-unpromoted", String(unpromotedCount));
  setTextWithPop("pulse-active", String(activeCount));
  setTextWithPop("pulse-done", String(doneCount));
  const elRate = document.getElementById("pulse-rate");
  if (elRate) elRate.textContent = rate;
}

function updateTaskMetrics() {
  const activeTasks = safeArray(state.taskBuckets.active);
  const completedTasks = safeArray(state.taskBuckets.completed);
  const archivedTasks = safeArray(state.taskBuckets.archived);
  const urgentCount = activeTasks.filter((t) => t.priority === "P0" || t.priority === "P1").length;
  const overdueCount = activeTasks.filter((t) => isOverdue(t)).length;
  const archivedDone = archivedTasks.filter((t) => t.status === "done").length;
  const totalDone = completedTasks.length + archivedDone;
  const totalAll = activeTasks.length + totalDone;
  const rateText = totalAll > 0 ? `整体达成率 ${Math.round((totalDone / totalAll) * 100)}%` : "含已归档完成项";

  setTextWithPop("metric-active", String(activeTasks.length));
  setTextWithPop("metric-urgent", String(urgentCount));
  setTextWithPop("metric-overdue", String(overdueCount));
  setTextWithPop("metric-done", String(totalDone));
  const mRate = document.getElementById("metric-rate");
  if (mRate) mRate.textContent = rateText;
}

/* ===== 导航与列表 ===== */
function switchView(view) {
  if (!viewCopy[view]) return;
  const prevView = state.view;
  state.view = view;
  document.querySelectorAll(".nav-item[data-view]").forEach((button) => {
    const active = button.dataset.view === view;
    button.classList.toggle("active", active);
    button.setAttribute("aria-current", active ? "page" : "false");
  });
  const inboxView = document.getElementById("view-inbox");
  const tasksView = document.getElementById("view-tasks");
  inboxView?.classList.toggle("hidden", view !== "inbox");
  tasksView?.classList.toggle("hidden", view !== "tasks");
  document.getElementById("btn-process-pending")?.classList.toggle("hidden", view !== "inbox");
  const pageKicker = document.getElementById("page-kicker");
  const pageTitle = document.getElementById("page-title");
  const pageSubtitle = document.getElementById("page-subtitle");
  if (pageKicker) pageKicker.textContent = viewCopy[view].kicker;
  if (pageTitle) pageTitle.textContent = viewCopy[view].title;
  if (pageSubtitle) pageSubtitle.textContent = viewCopy[view].subtitle;

  const globalSearch = document.getElementById("inbox-search");
  if (globalSearch) {
    globalSearch.placeholder = view === "inbox" ? "搜索收集箱..." : "搜索任务...";
    globalSearch.value = view === "inbox" ? state.inboxSearch : state.taskFilter;
  }

  const targetView = view === "inbox" ? inboxView : tasksView;
  replayAnimation(targetView, "view-entering");

  if (prevView !== view) {
    if (view === "inbox") {
      state.animateInboxList = true;
      renderInbox();
    } else if (view === "tasks") {
      state.animateTaskList = true;
      renderTasks();
    }
  }
}

function inboxMatchesFilter(item) {
  const query = state.inboxSearch.trim().toLowerCase();
  if (query) {
    const ai = parseAiResult(item.ai_result);
    const haystack = `${item.raw_text || ""}\n${ai?.title || ""}\n${ai?.summary || ""}\n${ai?.description || ""}`.toLowerCase();
    if (!haystack.includes(query)) return false;
  }
  const filter = state.inboxFilter;
  if (!filter || filter === "all") return true;
  if (filter === "unpromoted") {
    const { promoted, total } = promotionCounts(item);
    return promoted < total || ["pending", "queued", "processing", "failed", "partial", "unconfigured"].includes(item.ai_status);
  }
  if (filter === "image") return Boolean(item.image_path);
  if (filter === "attention") {
    return ["pending", "queued", "processing", "failed", "partial", "unconfigured"].includes(item.ai_status);
  }
  return true;
}

function renderInbox() {
  const list = document.getElementById("inbox-list");
  const count = document.getElementById("inbox-count");
  const navCount = document.getElementById("nav-inbox-count");
  const shown = state.inbox.filter(inboxMatchesFilter);
  if (count) {
    count.textContent = state.inboxFilter === "all" && !state.inboxSearch.trim()
      ? String(state.inbox.length)
      : String(shown.length);
  }
  if (navCount) navCount.textContent = String(state.inbox.length);
  updateWorkspacePulse();
  if (!list) return;
  if (!shown.length) {
    state.lastInboxItemsSig = "empty";
    state.animateInboxList = false;
    list.innerHTML = state.inbox.length
      ? '<div class="empty">当前筛选条件下没有记录<br /><strong>可切换上方「全部」查看所有收集记录</strong></div>'
      : '<div class="empty">还没有收集箱记录<br /><strong>按 Ctrl + Alt + Q 快速记录文字或粘贴截图</strong></div>';
    return;
  }

  const itemsSig = `${state.inboxFilter}:${state.inboxSearch.trim()}:` + shown.map((item) => {
    const { promoted, total } = promotionCounts(item);
    return `${item.id}:${item.ai_status}:${promoted}/${total}:${item.raw_text || ""}:${item.ai_result || ""}:${item.image_path ? 1 : 0}`;
  }).join("|");

  // If only selection changed (or 4s background poll had no inbox changes),
  // update `.active` in-place so the card's spring CSS transition plays smoothly without DOM recreation.
  if (!state.animateInboxList && itemsSig === state.lastInboxItemsSig && list.querySelectorAll(".inbox-row").length === shown.length) {
    list.querySelectorAll(".inbox-row").forEach((row) => {
      row.classList.toggle("active", Number(row.dataset.id) === state.selectedId);
    });
    return;
  }

  const shouldCascade = state.animateInboxList;
  state.animateInboxList = false;
  state.lastInboxItemsSig = itemsSig;

  let lastGroupHeading = null;
  const htmlChunks = [];

  shown.forEach((item, idx) => {
    const groupHeading = getRelativeDateHeading(item.created_at);
    if (groupHeading !== lastGroupHeading) {
      lastGroupHeading = groupHeading;
      htmlChunks.push(`<div class="inbox-date-header">${esc(groupHeading)}</div>`);
    }
    const { title, subtitle } = splitInboxCardCopy(item);
    const active = item.id === state.selectedId;
    const statusCls = item.ai_status || "pending";
    const enterCls = (shouldCascade || !state.knownInboxIds.has(item.id)) ? "card-enter" : "";
    const timeLabel = formatTime(item.created_at);
    htmlChunks.push(`
      <article class="inbox-row st-${esc(statusCls)} ${active ? "active" : ""} ${enterCls}" style="--i:${Math.min(idx, 12)}" data-id="${item.id}" tabindex="0" role="button" aria-label="打开记录：${esc(title)}">
        <div class="inbox-row-main">
          <div class="inbox-row-top">
            <div class="inbox-row-title">${esc(title)}</div>
            <time class="inbox-row-time" datetime="${esc(item.created_at)}">${esc(timeLabel)}</time>
          </div>
          <div class="inbox-row-sub">${esc(subtitle)}</div>
        </div>
      </article>`);
  });

  list.innerHTML = htmlChunks.join("");
  state.knownInboxIds = new Set(state.inbox.map((item) => item.id));

  list.querySelectorAll(".inbox-row").forEach((row) => {
    if (row.classList.contains("card-enter")) {
      row.addEventListener("animationend", () => row.classList.remove("card-enter"), { once: true });
    }
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

function taskMatchesQuickFilter(task) {
  const qf = state.taskQuickFilter;
  if (!qf || qf === "all") return true;
  if (qf === "P0" || qf === "P1") return task.priority === qf;
  if (qf === "overdue") return isOverdue(task);
  if (qf === "subtasks") return safeArray(task.subtasks).length > 0;
  return true;
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
  updateTaskMetrics();
  updateWorkspacePulse();

  const query = state.taskFilter.trim().toLowerCase();
  const shown = state.tasks.filter((task) => {
    if (query && !taskMatchesFilter(task, query)) return false;
    return taskMatchesQuickFilter(task);
  });

  if (navCount) navCount.textContent = String(counts.active + counts.completed);
  if (taskCount) taskCount.textContent = `${shown.length} 项任务`;
  if (!list) return;
  if (!shown.length) {
    state.lastTaskRenderSig = `empty:${state.taskView}:${query}:${state.taskQuickFilter}`;
    state.animateTaskList = false;
    const hasFilter = Boolean(query) || state.taskQuickFilter !== "all";
    const empty = state.taskView === "archived"
      ? "暂无已归档任务"
      : state.taskView === "completed"
        ? "暂无已完成任务"
        : "暂无进行中任务<br /><strong>从收集箱确认加入，或点击右上角新建正式任务</strong>";
    list.innerHTML = `<div class="empty">${hasFilter ? "没有匹配当前筛选条件的任务" : empty}</div>`;
    return;
  }

  const tasksSig = `${state.taskView}:${query}:${state.taskQuickFilter}:` + shown.map((task) => {
    const subtasks = safeArray(task.subtasks);
    const doneCount = subtasks.filter((s) => s.is_done).length;
    return `${task.id}:${task.title}:${task.status}:${task.priority}:${task.due_at || ""}:${task.progress || 0}:${doneCount}/${subtasks.length}:${isOverdue(task) ? 1 : 0}`;
  }).join("|");

  if (!state.animateTaskList && tasksSig === state.lastTaskRenderSig && list.querySelectorAll(".task").length === shown.length) {
    return;
  }

  const shouldCascade = state.animateTaskList;
  state.animateTaskList = false;
  state.lastTaskRenderSig = tasksSig;

  const priorityOptions = (current) => ["P0", "P1", "P2", "P3"]
    .map((priority) => `<option value="${priority}" ${priority === current ? "selected" : ""}>${priority}</option>`)
    .join("");

  list.innerHTML = shown.map((task, idx) => {
    const overdue = isOverdue(task);
    const subtasks = safeArray(task.subtasks);
    const doneCount = subtasks.filter((subtask) => subtask.is_done).length;
    const tags = safeArray(task.tags).slice(0, 4);
    const due = task.due_at ? `<span class="${overdue ? "overdue" : ""}">截止 ${esc(formatDateTime(task.due_at))}${overdue ? " · 已逾期" : ""}</span>` : "";
    const planned = task.planned_start ? `<span>计划 ${esc(formatDateTime(task.planned_start))}${task.planned_end ? ` – ${esc(formatTime(task.planned_end))}` : ""}</span>` : "";
    const description = task.description ? `<div class="task-description">${esc(task.description)}</div>` : "";
    const tagLine = tags.length ? `<span class="task-tags">${tags.map((tag) => `#${esc(tag)}`).join(" ")}</span>` : "";
    const progress = Number(task.progress) > 0 ? `<div class="progress" aria-label="进度 ${Math.min(Number(task.progress), 100)}%"><div class="progress-bar" style="width:${Math.min(Math.max(Number(task.progress) || 0, 0), 100)}%"></div></div>` : "";
    const restoreLabel = task.status === "done" ? "恢复到已完成" : "恢复到进行中";
    const prioClass = `prio-${esc(task.priority || "P2")}`;
    const enterCls = (shouldCascade || !state.knownTaskIds.has(task.id)) ? "card-enter" : "";
    return `
      <article class="task task-row task-row-${esc(taskViewFor(task))} ${prioClass} ${overdue ? "task-overdue" : ""} ${enterCls}" style="--i:${Math.min(idx, 12)}" data-id="${task.id}" tabindex="0" role="button" aria-label="编辑任务：${esc(task.title)}">
        <div class="task-main">
          <div class="task-head"><div class="title">${esc(task.title)}</div><span class="badge st-${esc(task.status)}">${esc(taskStatusText[task.status] || task.status)}</span></div>
          ${description}
          <div class="row task-meta">
            <label class="priority-field">优先级 <select class="task-priority" data-id="${task.id}" aria-label="修改优先级">${priorityOptions(task.priority)}</select></label>
            ${due}${planned}
            ${task.estimate_minutes ? `<span>预估 ${esc(task.estimate_minutes)} 分钟</span>` : ""}
            ${task.actual_minutes ? `<span>实际 ${esc(task.actual_minutes)} 分钟</span>` : ""}
            ${subtasks.length ? `<span>步骤 ${doneCount}/${subtasks.length}</span>` : ""}
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

  const allLoadedTasks = taskViews.flatMap((v) => state.taskBuckets[v] || []);
  state.knownTaskIds = new Set((allLoadedTasks.length ? allLoadedTasks : state.tasks).map((t) => t.id));

  list.querySelectorAll(".task").forEach((card) => {
    if (card.classList.contains("card-enter")) {
      card.addEventListener("animationend", () => card.classList.remove("card-enter"), { once: true });
    }
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
        const card = button.closest(".task");
        await runBusy(`status:${task.id}`, async () => {
          try {
            await Promise.all([
              invoke("update_task", { task: taskAfterAction(task, action) }),
              animateCardAction(card, action, action === "start" ? 140 : 185),
            ]);
            showToast(`任务已${taskStatusText[status] || status}`);
            await refreshAll();
          } catch (error) {
            card?.classList.remove(`card-acting-${action}`);
            showError(`状态更新失败：${errorText(error)}`);
          }
        });
      });
    });
  });

  list.querySelectorAll("[data-action=\"archive\"]").forEach((button) => {
    button.addEventListener("click", async (event) => {
      event.stopPropagation();
      const card = button.closest(".task");
      await runBusy(`archive:${button.dataset.id}`, async () => {
        try {
          await Promise.all([
            invoke("archive_task", { id: Number(button.dataset.id) }),
            animateCardAction(card, "archive", 185),
          ]);
          showToast("任务已归档");
          await refreshAll();
        } catch (error) {
          card?.classList.remove("card-acting-archive");
          showError(`归档失败：${errorText(error)}`);
        }
      });
    });
  });

  list.querySelectorAll("[data-action=\"unarchive\"]").forEach((button) => {
    button.addEventListener("click", async (event) => {
      event.stopPropagation();
      const card = button.closest(".task");
      await runBusy(`archive:${button.dataset.id}`, async () => {
        try {
          const task = state.tasks.find((item) => item.id === Number(button.dataset.id));
          await Promise.all([
            invoke("unarchive_task", { id: Number(button.dataset.id) }),
            animateCardAction(card, "unarchive", 185),
          ]);
          showToast(task?.status === "done" ? "任务已恢复到已完成" : "任务已恢复到进行中");
          await refreshAll();
        } catch (error) {
          card?.classList.remove("card-acting-unarchive");
          showError(`恢复任务失败：${errorText(error)}`);
        }
      });
    });
  });

  list.querySelectorAll("[data-action=\"delete\"]").forEach((button) => {
    button.addEventListener("click", async (event) => {
      event.stopPropagation();
      if (!window.confirm("确定删除该任务？删除后不会出现在任何列表中。")) return;
      const card = button.closest(".task");
      await runBusy(`delete-task:${button.dataset.id}`, async () => {
        try {
          await Promise.all([
            invoke("delete_task", { id: Number(button.dataset.id) }),
            animateCardAction(card, "delete", 185),
          ]);
          showToast("任务已删除");
          await refreshAll();
        } catch (error) {
          card?.classList.remove("card-acting-delete");
          showError(`删除任务失败：${errorText(error)}`);
        }
      });
    });
  });
}

function setTaskView(view) {
  if (!taskViews.includes(view)) return;
  if (state.taskView !== view) state.animateTaskList = true;
  state.taskView = view;
  state.tasks = state.taskBuckets[view] || [];
  renderTasks();
  refreshTasks().catch((error) => showError(`任务刷新失败：${errorText(error)}`));
}

/* ===== 收集箱四卡式检查器与草稿同步 ===== */
function selectedInboxItem() {
  return state.inbox.find((item) => item.id === state.selectedId) || null;
}

function draftKeyFor(item, index = state.selectedItemIndex) {
  return `${item.id}:${itemKeyFor(item, index) || `ui-item-${index}`}`;
}

function syncFriendlyDeadlineUi() {
  const dueInput = document.getElementById("f-due");
  const mainEl = document.getElementById("due-friendly-main");
  const subEl = document.getElementById("due-friendly-sub");
  const dateInput = document.getElementById("f-due-date");
  const timeInput = document.getElementById("f-due-time");
  if (!dueInput || !mainEl || !subEl) return;
  const raw = dueInput.value || "";
  const { main, sub } = formatFriendlyDeadline(raw);
  mainEl.textContent = main;
  subEl.textContent = sub;
  if (dateInput && timeInput) {
    if (raw && raw.includes("T")) {
      const [dPart, tPart] = raw.split("T");
      dateInput.value = dPart || "";
      timeInput.value = (tPart || "18:00").slice(0, 5);
    } else {
      dateInput.value = "";
      timeInput.value = "18:00";
    }
  }
}

function computePresetDueLocal(preset) {
  const now = new Date();
  const target = new Date(now.getFullYear(), now.getMonth(), now.getDate(), 18, 0, 0, 0);
  if (preset === "today-18") {
    target.setHours(18, 0, 0, 0);
  } else if (preset === "tomorrow-10") {
    target.setDate(target.getDate() + 1);
    target.setHours(10, 0, 0, 0);
  } else if (preset === "tomorrow-18") {
    target.setDate(target.getDate() + 1);
    target.setHours(18, 0, 0, 0);
  } else if (preset === "friday-18") {
    const day = target.getDay();
    const diff = day <= 5 ? (5 - day || 7) : (5 + 7 - day);
    target.setDate(target.getDate() + diff);
    target.setHours(18, 0, 0, 0);
  } else if (preset === "monday-10") {
    const day = target.getDay();
    const diff = day === 0 ? 1 : (8 - day);
    target.setDate(target.getDate() + diff);
    target.setHours(10, 0, 0, 0);
  }
  const yyyy = target.getFullYear();
  const mm = String(target.getMonth() + 1).padStart(2, "0");
  const dd = String(target.getDate()).padStart(2, "0");
  const hh = String(target.getHours()).padStart(2, "0");
  const min = String(target.getMinutes()).padStart(2, "0");
  return `${yyyy}-${mm}-${dd}T${hh}:${min}`;
}

function syncFriendlyPriorityUi() {
  const prioSelect = document.getElementById("f-priority");
  const labelEl = document.getElementById("priority-friendly-label");
  const wrapEl = document.getElementById("priority-picker-trigger") || prioSelect?.closest(".meta-priority");
  if (!prioSelect || !labelEl) return;
  const val = prioSelect.value || "P2";
  labelEl.textContent = priorityFriendlyText[val] || "中优先级";
  if (wrapEl) {
    wrapEl.classList.remove("prio-P0", "prio-P1", "prio-P2", "prio-P3");
    wrapEl.classList.add(`prio-${val}`);
  }
  document.querySelectorAll("#priority-popover .priority-option").forEach((btn) => {
    btn.classList.toggle("active", btn.dataset.prio === val);
  });
}

function syncSubtasksTextareaFromStepsDom() {
  const listEl = document.getElementById("draft-steps-list");
  const textarea = document.getElementById("f-subtasks");
  if (!listEl || !textarea) return;
  const lines = [...listEl.querySelectorAll(".draft-step-input")]
    .map((input) => input.value.trim())
    .filter(Boolean);
  textarea.value = lines.join("\n");
  markInboxDraftDirty();
}

function renderDraftStepsList() {
  const listEl = document.getElementById("draft-steps-list");
  const textarea = document.getElementById("f-subtasks");
  if (!listEl || !textarea) return;
  const item = selectedInboxItem();
  const dKey = item ? draftKeyFor(item) : "default";
  if (!state.checkedDraftSteps.has(dKey)) {
    state.checkedDraftSteps.set(dKey, new Set());
  }
  const checkedSet = state.checkedDraftSteps.get(dKey);

  const lines = textarea.value
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);

  if (!lines.length) {
    listEl.innerHTML = '<div class="draft-steps-empty">暂无拆解步骤，点击右上角「+ 添加步骤」补充执行清单</div>';
    return;
  }

  listEl.innerHTML = lines.map((stepText, idx) => {
    const isChecked = checkedSet.has(idx);
    return `
      <div class="draft-step-row ${isChecked ? "is-checked" : ""}" data-step-index="${idx}" role="listitem">
        <button class="step-circle-btn" type="button" aria-label="勾选步骤" aria-pressed="${isChecked}">
          <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <polyline points="20 6 9 17 4 12" />
          </svg>
        </button>
        <input class="draft-step-input" type="text" value="${esc(stepText)}" aria-label="执行步骤 ${idx + 1}" />
        <button class="step-remove-btn" type="button" title="移除此步骤" aria-label="移除步骤">×</button>
      </div>`;
  }).join("");

  listEl.querySelectorAll(".draft-step-row").forEach((row) => {
    const idx = Number(row.dataset.stepIndex);
    const circleBtn = row.querySelector(".step-circle-btn");
    const stepInput = row.querySelector(".draft-step-input");
    const removeBtn = row.querySelector(".step-remove-btn");

    circleBtn?.addEventListener("click", () => {
      if (checkedSet.has(idx)) checkedSet.delete(idx);
      else checkedSet.add(idx);
      const nowChecked = checkedSet.has(idx);
      row.classList.toggle("is-checked", nowChecked);
      circleBtn.setAttribute("aria-pressed", String(nowChecked));
    });

    stepInput?.addEventListener("input", () => {
      syncSubtasksTextareaFromStepsDom();
    });

    stepInput?.addEventListener("keydown", (event) => {
      if (event.isComposing || event.keyCode === 229) return;
      if (event.key === "Enter") {
        event.preventDefault();
        addDraftStep("");
      }
    });

    removeBtn?.addEventListener("click", () => {
      row.remove();
      syncSubtasksTextareaFromStepsDom();
      renderDraftStepsList();
    });
  });
}

function addDraftStep(initialText = "新执行步骤") {
  const textarea = document.getElementById("f-subtasks");
  if (!textarea) return;
  const current = textarea.value
    .split("\n")
    .map((s) => s.trim())
    .filter(Boolean);
  current.push(initialText || "新执行步骤");
  textarea.value = current.join("\n");
  markInboxDraftDirty();
  renderDraftStepsList();
  const inputs = document.querySelectorAll("#draft-steps-list .draft-step-input");
  const lastInput = inputs[inputs.length - 1];
  if (lastInput) {
    lastInput.focus();
    lastInput.select();
  }
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

function autoResizeHeroDesc() {
  const desc = document.getElementById("f-description");
  if (!desc) return;
  desc.style.height = "auto";
  desc.style.height = `${Math.max(desc.scrollHeight, 26)}px`;
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
  autoResizeHeroDesc();
  syncFriendlyDeadlineUi();
  syncFriendlyPriorityUi();
  renderDraftStepsList();
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
    description: ai?.summary || ai?.description || item.raw_text || "",
    tags: safeArray(ai?.tags).join(", "),
    subtasks: safeArray(ai?.subtasks).join("\n"),
  });
}

function updatePromotionUi(item, index) {
  const button = document.getElementById("btn-promote");
  const note = document.getElementById("f-item-state");
  const statusPill = document.getElementById("ai-status");
  const key = itemKeyFor(item, index);
  const promoted = isItemPromoted(item, index);
  const { promoted: promotedCount, total } = promotionCounts(item);
  const progress = document.getElementById("promotion-progress");

  if (progress) {
    if (total > 1) {
      progress.textContent = `${promotedCount}/${total} 已加入`;
      progress.classList.remove("hidden");
    } else {
      progress.textContent = "";
      progress.classList.add("hidden");
    }
  }

  if (statusPill) {
    if (promoted) {
      statusPill.textContent = "已加入任务";
      statusPill.className = "draft-state-pill promoted";
    } else {
      statusPill.textContent = statusText[item.ai_status] || item.ai_status || "待确认";
      statusPill.className = `draft-state-pill ${item.ai_status || "pending"}`;
    }
  }

  if (button) {
    const operationKey = key ? `promote:${item.id}:${key}` : null;
    const busyKey = button.dataset.busyKey;
    const busyForCurrentItem = !!busyKey && busyKey === operationKey && state.busy.has(busyKey);
    if (busyForCurrentItem) {
      button.disabled = true;
      button.setAttribute("aria-busy", "true");
      button.textContent = "加入中…";
    } else {
      if (busyKey) {
        delete button.dataset.busyKey;
        delete button.dataset.idleLabel;
      }
      button.removeAttribute("aria-busy");
      button.disabled = promoted || !key;
      button.textContent = promoted ? "已加入任务" : "加入任务";
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

function renderMissingInfoBar(activeAi) {
  const bar = document.getElementById("draft-missing-bar");
  const textEl = document.getElementById("draft-missing-text");
  if (!bar || !textEl) return;
  const missing = safeArray(activeAi?.missing_info).map((s) => String(s).trim()).filter(Boolean);
  if (!missing.length) {
    bar.classList.add("hidden");
    textEl.textContent = "";
    return;
  }
  const joined = missing.length === 2 ? `${missing[0]}与 ${missing[1]}` : missing.join("、");
  textEl.textContent = joined;
  bar.classList.remove("hidden");
}

function renderAiSummary(item, ai, items) {
  const box = document.getElementById("ai-summary");
  const chipsBox = document.getElementById("ai-chips");
  const activeAi = items[state.selectedItemIndex] || ai;
  renderMissingInfoBar(activeAi);

  if (!box) return;
  if (chipsBox) {
    chipsBox.innerHTML = "";
    chipsBox.classList.add("hidden");
  }
  const status = item.ai_status;
  if (status === "failed") {
    box.textContent = `解析失败：${item.ai_result || "未知错误"}\n可以点击右上角“↻”重试解析。`;
    box.classList.remove("hidden");
    return;
  }
  if (status === "unconfigured") {
    box.textContent = "尚未配置 API Key。打开左下角设置填写后，即可重新解析这条记录。";
    box.classList.remove("hidden");
    return;
  }
  if (["queued", "pending", "processing"].includes(status)) {
    box.textContent = status === "processing" ? "AI 正在整理这条记录，请稍候。" : "这条记录已排队，等待 AI 整理。";
    box.classList.remove("hidden");
    return;
  }
  if (status === "skipped") {
    box.textContent = "这条记录跳过了自动解析，可以在设置中开启后重新解析。";
    box.classList.remove("hidden");
    return;
  }
  if (!ai) {
    box.textContent = status === "partial" ? "结果待核对：解析输出不完整，请编辑草稿后再加入任务。" : "暂无可展示的 AI 解析结果。";
    box.classList.remove("hidden");
    return;
  }

  box.classList.add("hidden");
  if (chipsBox && activeAi) {
    const rawChips = [];
    const routedPersona = activeAi.persona || ai.persona;
    if (routedPersona && personaText[routedPersona]) {
      rawChips.push({ cls: "persona", html: `决策视角 <strong>${esc(personaText[routedPersona])}</strong>` });
    }
    if (activeAi.kind) rawChips.push({ cls: "", html: `类型 <strong>${esc(kindText[activeAi.kind] || activeAi.kind)}</strong>` });
    if (activeAi.priority) rawChips.push({ cls: "", html: `优先级 <strong>${esc(activeAi.priority)}</strong>` });
    if (typeof activeAi.confidence === "number" && activeAi.confidence > 0) {
      rawChips.push({ cls: "", html: `置信度 <strong>${Math.round(activeAi.confidence * 100)}%</strong>` });
    }
    if (activeAi.estimate_minutes) rawChips.push({ cls: "", html: `预估 <strong>${esc(activeAi.estimate_minutes)}m</strong>` });
    if (rawChips.length) {
      chipsBox.innerHTML = rawChips
        .map((c, idx) => `<span class="ai-chip ${c.cls}" style="--chip-i:${idx}">${c.html}</span>`)
        .join("");
      chipsBox.classList.remove("hidden");
    }
  }
}

async function renderDetail(id) {
  const item = state.inbox.find((entry) => entry.id === id);
  if (!item) return;
  const token = ++state.detailRenderToken;
  if (inspectorCloseTimer) {
    clearTimeout(inspectorCloseTimer);
    inspectorCloseTimer = null;
  }
  const panel = document.getElementById("detail-panel");
  const emptyEl = document.getElementById("detail-empty");
  const contentEl = document.getElementById("detail-content");
  panel.classList.remove("hidden");
  panel.setAttribute("aria-hidden", "false");
  emptyEl.classList.add("hidden");
  emptyEl.classList.remove("empty-entering");
  contentEl.classList.remove("hidden", "is-closing");

  if (state.lastInspectorId !== id) {
    state.lastInspectorId = id;
    replayAnimation(contentEl, "inspector-entering");
  }

  document.getElementById("detail-source").textContent = `${sourceText(item.source)} · ${formatDateTime(item.created_at)}${item.image_path ? " · 图片附件" : ""}`;
  document.getElementById("ai-model").textContent = item.ai_model ? `模型：${item.ai_model}` : "";
  document.getElementById("raw-text").textContent = item.raw_text || (item.image_path ? "（无文字说明，仅图片附件）" : "");

  const imageBlock = document.getElementById("detail-image");
  const image = document.getElementById("detail-img");
  if (item.image_path) {
    imageBlock.classList.remove("hidden");
    image.classList.remove("is-loaded");
    image.onload = () => image.classList.add("is-loaded");
    image.alt = "记录附件";
    try {
      image.src = await invoke("read_inbox_image", { id: item.id });
      if (image.complete && image.naturalWidth > 0) image.classList.add("is-loaded");
    } catch {
      if (token === state.detailRenderToken) imageBlock.classList.add("hidden");
    }
  } else {
    imageBlock.classList.add("hidden");
    image.classList.remove("is-loaded");
    image.onload = null;
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
      return `<option value="${index}" ${index === state.selectedItemIndex ? "selected" : ""}>${added ? "[已加入] " : ""}${esc(entry.title || `待办 ${index + 1}`)}</option>`;
    }).join("");
    const validIndex = Math.min(Math.max(state.selectedItemIndex, 0), Math.max(items.length - 1, 0));
    state.selectedItemIndex = validIndex;
    selector.value = String(validIndex);
    selector.onchange = () => {
      saveCurrentInboxDraft();
      state.selectedItemIndex = Number(selector.value);
      renderAiSummary(item, ai, items);
      fillDraftFromItem(items[state.selectedItemIndex], item, state.selectedItemIndex);
      updatePromotionUi(item, state.selectedItemIndex);
    };
  } else {
    selectorWrap.classList.add("hidden");
    selector.onchange = null;
    state.selectedItemIndex = 0;
  }
  const currentAi = items[state.selectedItemIndex] || ai;
  const reparsePersonaEl = document.getElementById("reparse-persona");
  if (reparsePersonaEl && document.activeElement !== reparsePersonaEl) {
    const detected = currentAi?.persona || ai?.persona;
    reparsePersonaEl.value = detected && personaText[detected]
      ? detected
      : (currentSettings?.default_persona || "auto");
  }
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
  state.lastInspectorId = null;
  state.selectedItemIndex = 0;
  state.detailRenderToken += 1;
  const panel = document.getElementById("detail-panel");
  const emptyEl = document.getElementById("detail-empty");
  const contentEl = document.getElementById("detail-content");
  panel?.setAttribute("aria-hidden", "true");
  renderInbox();

  if (inspectorCloseTimer) {
    clearTimeout(inspectorCloseTimer);
    inspectorCloseTimer = null;
  }

  contentEl?.classList.add("hidden");
  emptyEl?.classList.remove("hidden");
}

async function promoteSelected() {
  const item = selectedInboxItem();
  if (!item) return;
  const index = state.selectedItemIndex;
  const itemKey = itemKeyFor(item, index);
  if (isItemPromoted(item, index)) {
    showToast("该待办已经加入正式任务");
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
      showToast("已加入正式任务");
      await refreshAll();
    } catch (error) {
      showError(`加入任务失败：${errorText(error)}`);
    }
  }, "btn-promote", "加入中…", () => {
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
  if (modalCloseTimers.has(id)) {
    clearTimeout(modalCloseTimers.get(id));
    modalCloseTimers.delete(id);
  }
  state.modalId = id;
  state.modalLastFocus = document.activeElement;
  modal.classList.remove("hidden", "is-closing");
  modal.setAttribute("aria-hidden", "false");
  void modal.offsetWidth;
  modal.classList.add("is-open");
  setTimeout(() => {
    const target = preferredFocusId ? document.getElementById(preferredFocusId) : modal.querySelector("input, select, textarea, button");
    target?.focus();
  }, 40);
}

function closeModal(id = state.modalId) {
  const modal = document.getElementById(id);
  if (!modal) return;
  modal.setAttribute("aria-hidden", "true");
  modal.classList.remove("is-open");
  modal.classList.add("is-closing");
  if (state.modalId === id) {
    state.modalId = null;
    const target = state.modalLastFocus;
    state.modalLastFocus = null;
    if (target && typeof target.focus === "function") target.focus();
  }
  if (modalCloseTimers.has(id)) {
    clearTimeout(modalCloseTimers.get(id));
  }
  const timer = setTimeout(() => {
    modalCloseTimers.delete(id);
    modal.classList.add("hidden");
    modal.classList.remove("is-closing");
  }, 200);
  modalCloseTimers.set(id, timer);
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
  document.getElementById("e-meta").textContent = `创建于 ${formatDateTime(task.created_at)} · 来源 ${task.source_inbox_id ? `收集箱 #${task.source_inbox_id}` : "手动创建"}`;
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
  document.getElementById("e-meta").textContent = "直接创建正式任务，不经过收集箱";
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
    box.innerHTML = '<div class="subtask-empty">暂无执行步骤</div>';
    return;
  }
  box.innerHTML = subtasks.map((subtask, idx) => `
    <div class="subtask ${subtask.is_done ? "is-done" : ""}" style="--sub-i:${Math.min(idx, 10)}" data-id="${subtask.id}">
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
          showError(`更新步骤失败：${errorText(error)}`);
        }
      });
    });
  });
  box.querySelectorAll(".subtask-del").forEach((button) => {
    button.addEventListener("click", async () => {
      const subtaskId = Number(button.dataset.id);
      const row = button.closest(".subtask");
      await runBusy(`subtask-delete:${subtaskId}`, async () => {
        try {
          row?.classList.add("is-removing");
          await Promise.all([
            invoke("delete_subtask", { id: subtaskId }),
            new Promise((resolve) => setTimeout(resolve, 160)),
          ]);
          await refreshAll();
        } catch (error) {
          row?.classList.remove("is-removing");
          showError(`删除步骤失败：${errorText(error)}`);
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
      showError(`添加步骤失败：${errorText(error)}`);
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
  document.getElementById("s-api-key").value = "";
  document.getElementById("s-remove-key").checked = false;
  document.getElementById("s-key-state").textContent = settings.has_api_key ? "已保存 API Key" : "未保存 API Key";
  document.getElementById("s-base-url").value = settings.base_url || "";
  document.getElementById("s-model").value = settings.model || "";
  document.getElementById("s-vision-model").value = settings.vision_model || "";
  const personaSelect = document.getElementById("s-default-persona");
  if (personaSelect) personaSelect.value = settings.default_persona || "auto";
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
      default_persona: document.getElementById("s-default-persona")?.value || "auto",
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
        default_persona: settings.default_persona,
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
  if (!state.didAutoSelectInitial && state.selectedId == null && state.inbox.length > 0) {
    state.didAutoSelectInitial = true;
    state.selectedId = state.inbox[0].id;
    state.selectedItemIndex = 0;
    renderInbox();
    await renderDetail(state.selectedId);
    return;
  }
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
    return await state.refreshPromise;
  } finally {
    state.refreshPromise = null;
  }
}

async function subscribe(eventName, handler) {
  if (!listen) return;
  try {
    await listen(eventName, handler);
  } catch (error) {
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
  initTheme();

  // 顶部栏侧边栏折叠切换
  document.getElementById("btn-sidebar-toggle")?.addEventListener("click", () => {
    document.getElementById("app-shell")?.classList.toggle("sidebar-collapsed");
  });

  // 顶部栏全局搜索
  const globalSearch = document.getElementById("inbox-search");
  globalSearch?.addEventListener("input", (event) => {
    const val = event.target.value;
    if (state.view === "inbox") {
      state.inboxSearch = val;
      renderInbox();
    } else {
      state.taskFilter = val;
      const taskFilterEl = document.getElementById("task-filter");
      if (taskFilterEl) taskFilterEl.value = val;
      renderTasks();
    }
  });

  document.getElementById("btn-pane-search")?.addEventListener("click", () => {
    globalSearch?.focus();
    globalSearch?.select();
  });

  // 顶部栏 ••• 更多菜单
  const moreBtn = document.getElementById("btn-more-menu");
  const morePopover = document.getElementById("titlebar-more-popover");
  moreBtn?.addEventListener("click", (event) => {
    event.stopPropagation();
    const isHidden = morePopover?.classList.toggle("hidden");
    moreBtn.setAttribute("aria-expanded", String(!isHidden));
  });
  document.addEventListener("click", (event) => {
    if (morePopover && !morePopover.classList.contains("hidden") && !event.target.closest(".titlebar-more-wrap")) {
      morePopover.classList.add("hidden");
      moreBtn?.setAttribute("aria-expanded", "false");
    }
  });

  document.getElementById("btn-theme-toggle")?.addEventListener("click", () => {
    const current = document.documentElement.getAttribute("data-theme") || "light";
    applyTheme(current === "dark" ? "light" : "dark");
    morePopover?.classList.add("hidden");
  });

  document.querySelectorAll(".nav-item[data-view]").forEach((button) => {
    button.addEventListener("click", () => switchView(button.dataset.view));
  });

  document.querySelectorAll("[data-inbox-filter]").forEach((button) => {
    button.addEventListener("click", () => {
      const nextFilter = button.dataset.inboxFilter || "all";
      if (state.inboxFilter !== nextFilter) state.animateInboxList = true;
      state.inboxFilter = nextFilter;
      document.querySelectorAll("[data-inbox-filter]").forEach((el) => {
        el.classList.toggle("active", el === button);
      });
      renderInbox();
    });
  });

  document.querySelectorAll("[data-task-quick]").forEach((button) => {
    button.addEventListener("click", () => {
      const nextQuick = button.dataset.taskQuick || "all";
      if (state.taskQuickFilter !== nextQuick) state.animateTaskList = true;
      state.taskQuickFilter = nextQuick;
      document.querySelectorAll("[data-task-quick]").forEach((el) => {
        el.classList.toggle("active", el === button);
      });
      renderTasks();
    });
  });

  document.getElementById("btn-refresh").addEventListener("click", () => {
    morePopover?.classList.add("hidden");
    refreshAll().catch((error) => showError(`刷新失败：${errorText(error)}`));
  });

  document.getElementById("btn-process-pending").addEventListener("click", async () => {
    morePopover?.classList.add("hidden");
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
    if (globalSearch && state.view === "tasks") globalSearch.value = event.target.value;
    renderTasks();
  });

  // 草稿 Hero 卡截止日期与优先级浮层交互
  const dueTrigger = document.getElementById("due-picker-trigger");
  const duePopover = document.getElementById("due-popover");
  const dueInput = document.getElementById("f-due");
  const dueDateInput = document.getElementById("f-due-date");
  const dueTimeInput = document.getElementById("f-due-time");
  const prioTrigger = document.getElementById("priority-picker-trigger");
  const prioPopover = document.getElementById("priority-popover");
  const prioSelect = document.getElementById("f-priority");

  function closeMetaPopovers() {
    duePopover?.classList.add("hidden");
    dueTrigger?.setAttribute("aria-expanded", "false");
    prioPopover?.classList.add("hidden");
    prioTrigger?.setAttribute("aria-expanded", "false");
  }

  dueTrigger?.addEventListener("click", (event) => {
    event.stopPropagation();
    const willOpen = duePopover?.classList.contains("hidden");
    closeMetaPopovers();
    if (willOpen && duePopover) {
      syncFriendlyDeadlineUi();
      duePopover.classList.remove("hidden");
      dueTrigger.setAttribute("aria-expanded", "true");
    }
  });

  duePopover?.addEventListener("click", (event) => {
    event.stopPropagation();
  });

  document.querySelectorAll("[data-due-preset]").forEach((chip) => {
    chip.addEventListener("click", () => {
      if (!dueInput) return;
      dueInput.value = computePresetDueLocal(chip.dataset.duePreset);
      syncFriendlyDeadlineUi();
      markInboxDraftDirty();
      closeMetaPopovers();
    });
  });

  function applyCustomDateAndTime() {
    if (!dueInput || !dueDateInput) return;
    const dVal = dueDateInput.value;
    const tVal = (dueTimeInput?.value || "18:00").slice(0, 5);
    if (!dVal) {
      dueInput.value = "";
    } else {
      dueInput.value = `${dVal}T${tVal}`;
    }
    syncFriendlyDeadlineUi();
    markInboxDraftDirty();
  }

  dueDateInput?.addEventListener("input", applyCustomDateAndTime);
  dueDateInput?.addEventListener("change", applyCustomDateAndTime);
  dueTimeInput?.addEventListener("input", () => {
    if (dueDateInput && !dueDateInput.value) {
      const now = new Date();
      const yyyy = now.getFullYear();
      const mm = String(now.getMonth() + 1).padStart(2, "0");
      const dd = String(now.getDate()).padStart(2, "0");
      dueDateInput.value = `${yyyy}-${mm}-${dd}`;
    }
    applyCustomDateAndTime();
  });

  document.getElementById("btn-due-clear")?.addEventListener("click", () => {
    if (dueInput) dueInput.value = "";
    if (dueDateInput) dueDateInput.value = "";
    syncFriendlyDeadlineUi();
    markInboxDraftDirty();
    closeMetaPopovers();
  });

  document.getElementById("btn-due-done")?.addEventListener("click", () => {
    if (dueDateInput?.value) applyCustomDateAndTime();
    closeMetaPopovers();
  });

  prioTrigger?.addEventListener("click", (event) => {
    event.stopPropagation();
    const willOpen = prioPopover?.classList.contains("hidden");
    closeMetaPopovers();
    if (willOpen && prioPopover) {
      prioPopover.classList.remove("hidden");
      prioTrigger.setAttribute("aria-expanded", "true");
    }
  });

  document.querySelectorAll("#priority-popover .priority-option").forEach((optionBtn) => {
    optionBtn.addEventListener("click", (event) => {
      event.stopPropagation();
      if (prioSelect && optionBtn.dataset.prio) {
        prioSelect.value = optionBtn.dataset.prio;
        syncFriendlyPriorityUi();
        markInboxDraftDirty();
      }
      closeMetaPopovers();
    });
  });

  document.addEventListener("click", (event) => {
    if (!event.target.closest("#due-popover-wrap") && !event.target.closest("#priority-popover-wrap")) {
      closeMetaPopovers();
    }
  });

  dueInput?.addEventListener("input", syncFriendlyDeadlineUi);
  dueInput?.addEventListener("change", syncFriendlyDeadlineUi);
  prioSelect?.addEventListener("change", syncFriendlyPriorityUi);
  document.getElementById("f-description")?.addEventListener("input", autoResizeHeroDesc);
  document.getElementById("f-subtasks")?.addEventListener("input", renderDraftStepsList);

  document.getElementById("btn-add-draft-step")?.addEventListener("click", () => {
    addDraftStep("补充执行步骤");
  });

  // 原始记录「更多属性」折叠抽屉
  const morePropsBtn = document.getElementById("btn-toggle-more-props");
  const morePropsDrawer = document.getElementById("more-props-drawer");
  morePropsBtn?.addEventListener("click", () => {
    const hidden = morePropsDrawer?.classList.toggle("hidden");
    morePropsBtn.setAttribute("aria-expanded", String(!hidden));
  });

  document.getElementById("btn-promote").addEventListener("click", promoteSelected);
  document.getElementById("btn-delete-inbox").addEventListener("click", async () => {
    const item = selectedInboxItem();
    if (!item || !window.confirm("只会在收集箱中忽略这条记录，不会删除正式任务中的关联记录。继续？")) return;
    const activeRow = document.querySelector(`.inbox-row[data-id="${item.id}"]`);
    await runBusy(`delete-inbox:${item.id}`, async () => {
      try {
        await Promise.all([
          invoke("delete_inbox", { id: item.id }),
          animateCardAction(activeRow, "delete", 180),
        ]);
        state.inboxDrafts.delete(draftKeyFor(item));
        closeInspector();
        showToast("记录已忽略");
        await refreshAll();
      } catch (error) {
        activeRow?.classList.remove("card-acting-delete");
        showError(`忽略记录失败：${errorText(error)}`);
      }
    }, "btn-delete-inbox", "处理中…");
  });

  document.getElementById("btn-reparse").addEventListener("click", async () => {
    const item = selectedInboxItem();
    if (!item) return;
    const persona = document.getElementById("reparse-persona")?.value || "auto";
    await runBusy(`reparse:${item.id}`, async () => {
      try {
        await invoke("parse_inbox_item", { id: item.id, persona });
        showToast(`已按「${personaText[persona] || "智能路由"}」重新加入解析队列`);
        await refreshAll();
      } catch (error) {
        showError(`重新解析失败：${errorText(error)}`);
      }
    }, "btn-reparse", "…");
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
    document.getElementById(id)?.addEventListener("input", markInboxDraftDirty);
    document.getElementById(id)?.addEventListener("change", markInboxDraftDirty);
  });

  attachModalEvents();
  document.addEventListener("keydown", handleKeydown);
  await subscribe("inbox-updated", () => refreshAll().catch((error) => showError(`收集箱刷新失败：${errorText(error)}`)));
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
