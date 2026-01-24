# Pulseboard (Rust + WASM)

A collaborative whiteboard with a Rust WebSocket server and a WebAssembly client.

## Run it

Build the client (repeat when client code changes):

```sh
cargo install wasm-pack
wasm-pack build client --target web --out-dir ../public/pkg
```

Start the server:

```sh
cargo run -p pfboard_server
```

Open http://localhost:3000 in multiple tabs or devices on the same network.
