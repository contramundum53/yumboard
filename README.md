# YumBoard

A simple collaborative whiteboard.

## Run the Server

```sh
cargo install wasm-pack
wasm-pack build client --release --target web --out-dir ../public/pkg
cargo run --release -p yumboard_server --sessions-dir ./sessions --port 3000
```

Then, open http://localhost:3000 to create a new whiteboard.
