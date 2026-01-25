# YumBoard

A simple collaborative whiteboard.

## Run the Server

```sh
cargo install wasm-pack
wasm-pack build client --release --target web --out-dir ../public/pkg
cargo run --release -p yumboard_server --sessions-dir ./sessions
```