/* global window, document, FileReader */
"use strict";

const tauri = typeof window !== "undefined" ? window.__TAURI__ : null;
const invoke = tauri?.core?.invoke
  ? (...args) => tauri.core.invoke(...args)
  : async () => { throw new Error("Tauri API 不可用"); };
const listen = tauri?.event?.listen
  ? (...args) => tauri.event.listen(...args)
  : null;

let pendingImage = null;
let saving = false;
let clearImgTimer = null;
let hideWindowTimer = null;

function openQuickStage() {
  const stage = document.getElementById("quick-stage");
  if (!stage) return;
  if (hideWindowTimer) {
    clearTimeout(hideWindowTimer);
    hideWindowTimer = null;
  }
  stage.classList.remove("closing");
  if (stage.classList.contains("active")) return;
  window.requestAnimationFrame(() => {
    stage.classList.add("active");
  });
}

if (typeof window !== "undefined") {
  window.__onQuickOpen = () => {
    syncQuickTheme();
    openQuickStage();
  };
  window.__onQuickClose = () => {
    void hideQuickWindow();
  };
}

function syncQuickTheme() {
  let target = "light";
  try {
    const saved = window.localStorage?.getItem("taskvault-theme") || "light";
    target = saved === "dark" ? "dark" : "light";
  } catch {
    target = "light";
  }
  if (document.documentElement.getAttribute("data-theme") !== target) {
    document.documentElement.setAttribute("data-theme", target);
  }
}

function setStatus(message, isError = false) {
  const status = document.getElementById("status");
  status.textContent = message;
  status.style.color = isError ? "#f43f5e" : "";
}

function setSaveBusy(busy) {
  const button = document.getElementById("btn-save");
  button.disabled = busy;
  button.setAttribute("aria-busy", String(busy));
  button.textContent = busy ? "保存中…" : "✨ 保存记录";
}

function setImage(dataUrl) {
  if (clearImgTimer) {
    clearTimeout(clearImgTimer);
    clearImgTimer = null;
  }
  pendingImage = dataUrl;
  const preview = document.getElementById("img-preview");
  document.getElementById("img-thumb").src = dataUrl;
  preview.classList.remove("hidden", "is-leaving");
  setStatus("已附加截图");
}

function clearImage(immediate = false) {
  pendingImage = null;
  const preview = document.getElementById("img-preview");
  if (clearImgTimer) {
    clearTimeout(clearImgTimer);
    clearImgTimer = null;
  }
  if (immediate === true || !preview || preview.classList.contains("hidden")) {
    document.getElementById("img-thumb").src = "";
    preview?.classList.add("hidden");
    preview?.classList.remove("is-leaving");
    return;
  }
  preview.classList.add("is-leaving");
  clearImgTimer = setTimeout(() => {
    clearImgTimer = null;
    if (pendingImage) return;
    document.getElementById("img-thumb").src = "";
    preview.classList.add("hidden");
    preview.classList.remove("is-leaving");
  }, 180);
}

async function save() {
  if (saving) return;
  const input = document.getElementById("task-input");
  const text = input.value.trim();
  if (!text && !pendingImage) {
    setStatus("先输入内容或附加一张图片");
    input.focus();
    return;
  }
  saving = true;
  setSaveBusy(true);
  setStatus("正在保存到记录区…");
  const personaSelect = document.getElementById("quick-persona");
  const persona = personaSelect?.value ? personaSelect.value : null;
  try {
    if (pendingImage) {
      const comma = pendingImage.indexOf(",");
      const imageBase64 = comma >= 0 ? pendingImage.slice(comma + 1) : pendingImage;
      await invoke("capture_inbox_image", { text: text || null, imageBase64, persona });
    } else {
      await invoke("capture_inbox", { text, source: "hotkey", persona });
    }
    input.value = "";
    if (personaSelect) personaSelect.value = "";
    clearImage(true);
    setStatus("已保存");
    await hideQuickWindow();
  } catch (error) {
    setStatus(`保存失败：${error?.message || error}`, true);
  } finally {
    saving = false;
    setSaveBusy(false);
  }
}

function readPasteAsDataUrl(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(reader.error || new Error("无法读取图片"));
    reader.readAsDataURL(file);
  });
}

async function hideQuickWindow() {
  const stage = document.getElementById("quick-stage");
  if (stage && stage.classList.contains("active")) {
    stage.classList.remove("active");
    stage.classList.add("closing");
    await new Promise((resolve) => {
      hideWindowTimer = setTimeout(resolve, 200);
    });
    hideWindowTimer = null;
  }
  try {
    await invoke("hide_window", { label: "quick" });
    stage?.classList.remove("closing");
  } catch (error) {
    stage?.classList.remove("closing");
    stage?.classList.add("active");
    setStatus(`隐藏失败：${error?.message || error}`, true);
  }
}

if (typeof document !== "undefined") {
  document.addEventListener("DOMContentLoaded", () => {
    syncQuickTheme();
    if (!document.hidden) openQuickStage();
    window.addEventListener("focus", syncQuickTheme);
    window.addEventListener("storage", syncQuickTheme);
    const input = document.getElementById("task-input");
    document.getElementById("btn-save").addEventListener("click", save);
    document.getElementById("btn-cancel").addEventListener("click", hideQuickWindow);
    document.getElementById("btn-remove-img").addEventListener("click", () => clearImage(false));
    input.addEventListener("input", () => {
      if (document.getElementById("status").textContent === "先输入内容或附加一张图片") setStatus("");
    });

    input.addEventListener("paste", async (event) => {
      const file = [...(event.clipboardData?.items || [])]
        .find((item) => item.kind === "file" && item.type.startsWith("image/"))
        ?.getAsFile();
      if (!file) return;
      event.preventDefault();
      try {
        setImage(await readPasteAsDataUrl(file));
      } catch (error) {
        setStatus(`读取截图失败：${error?.message || error}`, true);
      }
    });

    if (listen) {
      listen("tauri://drag-drop", async (event) => {
        const paths = event.payload?.paths || [];
        const imagePath = paths.find((path) => /\.(png|jpe?g|webp|gif)$/i.test(path));
        if (!imagePath) return;
        try {
          setImage(await invoke("read_image_file_base64", { path: imagePath }));
        } catch (error) {
          setStatus(`读取图片失败：${error?.message || error}`, true);
        }
      }).catch?.(() => {});
    }

    input.addEventListener("keydown", async (event) => {
      if (event.isComposing || event.keyCode === 229) return;
      if (event.key === "Enter" && !event.shiftKey) {
        event.preventDefault();
        await save();
      } else if (event.key === "Escape") {
        event.preventDefault();
        await hideQuickWindow();
      }
    });
    setTimeout(() => { input.focus(); input.select(); }, 30);
  });
}
