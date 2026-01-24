import init from "./pkg/pfboard_client.js";

init().catch((err) => {
  console.error("Failed to start WASM client", err);
});
