/* global window */
const invoke = window.__TAURI__.core.invoke;

async function save() {
  const el = document.getElementById("task-input");
  const text = el.value.trim();
  if (!text) return;
  document.getElementById("status").textContent = "保存中…";
  try {
    await invoke("capture_inbox", { text, source: "hotkey" });
    el.value = "";
    await invoke("hide_window", { label: "quick" });
  } catch (e) {
    document.getElementById("status").textContent = `失败：${e}`;
  }
}

document.addEventListener("DOMContentLoaded", () => {
  const el = document.getElementById("task-input");
  document.getElementById("btn-save").onclick = save;
  document.getElementById("btn-cancel").onclick = () => invoke("hide_window", { label: "quick" });

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
