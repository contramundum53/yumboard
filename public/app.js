import init from "./pkg/yumboard_client.js";

// If an iOS devtools extension injects Eruda, it may require explicit init.
// Do this opportunistically (and safely) so debug logging doesn't crash the app.
if (window.eruda && typeof window.eruda.init === "function") {
  if (!window.__yumboard_eruda_inited) {
    window.__yumboard_eruda_inited = true;
    try {
      window.eruda.init();
    } catch (e) {
      // Best-effort only.
    }
  }
}

window.addEventListener("error", (event) => {
  const yumboardMark = window.__yumboard_last_mark || null;
  console.error(`window.error mark=${yumboardMark || "null"}`, {
    message: event.message,
    filename: event.filename,
    lineno: event.lineno,
    colno: event.colno,
    error: event.error,
    yumboardMark,
  });
});

window.addEventListener("unhandledrejection", (event) => {
  const yumboardMark = window.__yumboard_last_mark || null;
  console.error(`window.unhandledrejection mark=${yumboardMark || "null"}`, {
    reason: event.reason,
    yumboardMark,
  });
});

const ua = navigator.userAgent || "";
const isChrome =
  (ua.includes("Chrome") && !ua.includes("Edg") && !ua.includes("OPR")) ||
  ua.includes("CriOS");
if (isChrome) {
  document.body.classList.add("is-chrome");
}

init().catch((err) => {
  console.error("Failed to start WASM client", err);
});
