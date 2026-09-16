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

function setStatus(message, isError = false) {
  const status = document.getElementById("status");
  status.textContent = message;
  status.style.color = isError ? "#ef767b" : "";
}

function setSaveBusy(busy) {
  const button = document.getElementById("btn-save");
  button.disabled = busy;
  button.setAttribute("aria-busy", String(busy));
  button.textContent = busy ? "保存中…" : "保存记录";
}

function setImage(dataUrl) {
  pendingImage = dataUrl;
  document.getElementById("img-thumb").src = dataUrl;
  document.getElementById("img-preview").classList.remove("hidden");
  setStatus("已附加截图");
}

function clearImage() {
  pendingImage = null;
  document.getElementById("img-thumb").src = "";
  document.getElementById("img-preview").classList.add("hidden");
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
  try {
    if (pendingImage) {
      const comma = pendingImage.indexOf(",");
      const imageBase64 = comma >= 0 ? pendingImage.slice(comma + 1) : pendingImage;
      await invoke("capture_inbox_image", { text: text || null, imageBase64 });
    } else {
      await invoke("capture_inbox", { text, source: "hotkey" });
    }
    input.value = "";
    clearImage();
    setStatus("已保存");
    await invoke("hide_window", { label: "quick" });
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
  try {
    await invoke("hide_window", { label: "quick" });
  } catch (error) {
    setStatus(`隐藏失败：${error?.message || error}`, true);
  }
}

if (typeof document !== "undefined") {
  document.addEventListener("DOMContentLoaded", () => {
    const input = document.getElementById("task-input");
    document.getElementById("btn-save").addEventListener("click", save);
    document.getElementById("btn-cancel").addEventListener("click", hideQuickWindow);
    document.getElementById("btn-remove-img").addEventListener("click", clearImage);
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
