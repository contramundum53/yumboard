import init from "./pkg/yumboard_client.js";

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
