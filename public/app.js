import init from "./pkg/yumboard_client.js";

window.addEventListener("error", (event) => {
  const yumboardMark = window.__yumboard_last_mark || null;
  console.error("window.error", {
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
  console.error("window.unhandledrejection", {
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
