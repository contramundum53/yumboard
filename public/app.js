import init from "./pkg/yumboard_client.js";

init().catch((err) => {
  console.error("Failed to start WASM client", err);
});
