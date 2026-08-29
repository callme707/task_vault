/* global window */
const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

// 待保存的截图（data URL）。一条记录一张图，后贴的替换先贴的。
let pendingImage = null;

function setImage(dataUrl) {
  pendingImage = dataUrl;
  document.getElementById("img-thumb").src = dataUrl;
  document.getElementById("img-preview").classList.remove("hidden");
}

function clearImage() {
  pendingImage = null;
  document.getElementById("img-thumb").src = "";
  document.getElementById("img-preview").classList.add("hidden");
}

function setStatus(msg) {
  document.getElementById("status").textContent = msg;
}

async function save() {
  const el = document.getElementById("task-input");
  const text = el.value.trim();
  if (!text && !pendingImage) return;
  setStatus("保存中…");
  try {
    if (pendingImage) {
      const base64 = pendingImage.slice(pendingImage.indexOf(",") + 1);
      await invoke("capture_inbox_image", { text: text || null, imageBase64: base64 });
    } else {
      await invoke("capture_inbox", { text, source: "hotkey" });
    }
    el.value = "";
    clearImage();
    setStatus("");
    await invoke("hide_window", { label: "quick" });
  } catch (e) {
    setStatus(`失败：${e}`);
  }
}

async function readPasteAsDataUrl(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

document.addEventListener("DOMContentLoaded", () => {
  const el = document.getElementById("task-input");
  document.getElementById("btn-save").onclick = save;
  document.getElementById("btn-cancel").onclick = () => invoke("hide_window", { label: "quick" });
  document.getElementById("btn-remove-img").onclick = clearImage;

  // 粘贴截图：clipboard 里的图片文件转 data URL 预览
  el.addEventListener("paste", async (event) => {
    const file = [...(event.clipboardData?.items || [])]
      .find((it) => it.kind === "file" && it.type.startsWith("image/"))
      ?.getAsFile();
    if (!file) return;
    event.preventDefault();
    try {
      setImage(await readPasteAsDataUrl(file));
      setStatus("已附加截图");
    } catch (e) {
      setStatus(`读取截图失败：${e}`);
    }
  });

  // 拖拽图片文件到窗口（Tauri 拦截了 HTML5 drop，走 tauri://drag-drop 事件拿路径）
  if (listen) {
    listen("tauri://drag-drop", async (event) => {
      const paths = event.payload?.paths || [];
      const imgPath = paths.find((p) => /\.(png|jpe?g|webp|gif)$/i.test(p));
      if (!imgPath) return;
      try {
        setImage(await invoke("read_image_file_base64", { path: imgPath }));
        setStatus("已附加图片");
      } catch (e) {
        setStatus(`读取图片失败：${e}`);
      }
    });
  }

  el.addEventListener("keydown", async (event) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      await save();
    } else if (event.key === "Escape") {
      event.preventDefault();
      await invoke("hide_window", { label: "quick" });
    }
  });
  setTimeout(() => { el.focus(); el.select(); }, 30);
});
