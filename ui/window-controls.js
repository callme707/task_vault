/* global window, document, Element */
"use strict";

// Tauri injects the drag-region behavior for data-tauri-drag-region. This
// module wires the explicit controls and supplies a browser-safe fallback for
// fixtures that expose a partial window object without Tauri internals.
(() => {
  const DESKTOP_ONLY = "（仅桌面版可用）";
  const NATIVE_BRIDGE = typeof window !== "undefined"
    && typeof window.__TAURI_INTERNALS__?.invoke === "function";

  let initialized = false;

  function firstById(...ids) {
    return ids.map((id) => document.getElementById(id)).find(Boolean) || null;
  }

  function resolveCurrentWindow() {
    const tauriWindow = typeof window !== "undefined" ? window.__TAURI__?.window : null;
    if (!tauriWindow || typeof tauriWindow.getCurrentWindow !== "function") return null;
    try {
      return tauriWindow.getCurrentWindow();
    } catch (error) {
      console.warn("无法获取当前桌面窗口", error);
      return null;
    }
  }

  function markDesktopOnly(button, label) {
    if (!button) return;
    button.disabled = true;
    button.setAttribute("aria-disabled", "true");
    button.dataset.desktopOnly = "true";
    const title = button.getAttribute("title") || label;
    if (!title.includes(DESKTOP_ONLY)) button.setAttribute("title", `${title}${DESKTOP_ONLY}`);
  }

  function reportActionFailure(button, label, error) {
    console.warn(`${label}仅支持桌面版窗口控制`, error);
    markDesktopOnly(button, label);
  }

  function setMaximizeState(button, maximized) {
    if (!button) return;
    const label = maximized ? "还原窗口" : "最大化窗口";
    button.dataset.windowState = maximized ? "maximized" : "normal";
    button.setAttribute("aria-label", label);
    button.setAttribute("title", label);
    button.setAttribute("aria-pressed", String(maximized));
    button.classList.toggle("is-maximized", maximized);

    const text = button.querySelector("[data-window-maximize-label]");
    if (text) text.textContent = label;
  }

  function bindSimpleAction(button, currentWindow, method, label) {
    if (!button) return;
    if (!currentWindow || typeof currentWindow[method] !== "function") {
      markDesktopOnly(button, label);
      return;
    }

    button.addEventListener("click", () => {
      if (button.disabled) return;
      Promise.resolve()
        .then(() => currentWindow[method]())
        .catch((error) => reportActionFailure(button, label, error));
    });
  }

  function bindMaximize(button, currentWindow) {
    if (!button) return;
    if (!currentWindow || typeof currentWindow.toggleMaximize !== "function") {
      markDesktopOnly(button, "最大化窗口");
      return;
    }

    button.addEventListener("click", () => {
      if (button.disabled) return;
      Promise.resolve()
        .then(() => currentWindow.toggleMaximize())
        .then(() => syncMaximizeState(button, currentWindow))
        .catch((error) => reportActionFailure(button, "最大化窗口", error));
    });
    void syncMaximizeState(button, currentWindow);
  }

  async function syncMaximizeState(button, currentWindow) {
    if (!button || !currentWindow || typeof currentWindow.isMaximized !== "function") return;
    try {
      setMaximizeState(button, await currentWindow.isMaximized());
    } catch (error) {
      // isMaximized is included by core:window:default. Keep the action usable
      // for a lightweight fixture that only implements toggleMaximize().
      console.warn("无法读取窗口最大化状态", error);
    }
  }

  function isInteractiveTarget(target) {
    return typeof Element !== "undefined"
      && target instanceof Element
      && Boolean(target.closest("a,button,input,select,textarea,label,summary,[contenteditable='true'],[role='button'],[role='link']"));
  }

  function bindDragFallback(region, currentWindow) {
    if (!region || NATIVE_BRIDGE || !currentWindow || typeof currentWindow.startDragging !== "function") return;

    region.addEventListener("mousedown", (event) => {
      if (event.button !== 0 || event.detail !== 1 || isInteractiveTarget(event.target)) return;
      event.preventDefault();
      Promise.resolve()
        .then(() => currentWindow.startDragging())
        .catch((error) => console.warn("窗口拖动失败", error));
    });

    region.addEventListener("dblclick", (event) => {
      if (event.button !== 0 || isInteractiveTarget(event.target) || typeof currentWindow.toggleMaximize !== "function") return;
      event.preventDefault();
      Promise.resolve()
        .then(() => currentWindow.toggleMaximize())
        .catch((error) => console.warn("双击最大化失败", error));
    });
  }

  function init() {
    if (initialized || typeof document === "undefined") return;
    initialized = true;

    const currentWindow = resolveCurrentWindow();
    const minimize = firstById("window-minimize", "win-minimize");
    const maximize = firstById("window-maximize", "win-maximize");
    const close = firstById("window-close", "win-close");
    const dragRegion = firstById("window-drag-region", "titlebar-drag")
      || document.querySelector(".app-titlebar");

    bindSimpleAction(minimize, currentWindow, "minimize", "最小化窗口");
    bindMaximize(maximize, currentWindow);
    bindSimpleAction(close, currentWindow, "close", "关闭窗口");

    if (dragRegion) {
      // "deep" lets the titlebar text drag the window while Tauri still
      // blocks clickable descendants such as the three control buttons.
      if (!dragRegion.hasAttribute("data-tauri-drag-region")) {
        dragRegion.setAttribute("data-tauri-drag-region", "deep");
      } else if (["", "true"].includes(dragRegion.getAttribute("data-tauri-drag-region"))) {
        dragRegion.setAttribute("data-tauri-drag-region", "deep");
      }
      bindDragFallback(dragRegion, currentWindow);
    }

    if (currentWindow && typeof currentWindow.onResized === "function") {
      Promise.resolve(currentWindow.onResized(() => syncMaximizeState(maximize, currentWindow)))
        .catch((error) => console.warn("无法监听窗口大小变化", error));
    }
    if (typeof window !== "undefined") {
      window.addEventListener("resize", () => syncMaximizeState(maximize, currentWindow));
    }
  }

  if (typeof document !== "undefined") {
    if (document.readyState === "loading") {
      document.addEventListener("DOMContentLoaded", init, { once: true });
    } else {
      init();
    }
  }
})();
