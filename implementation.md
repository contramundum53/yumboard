# YumBoard: Implementation Notes

YumBoard is a collaborative whiteboard that runs entirely in the browser (Canvas + WebAssembly) and
syncs edits in real time via WebSockets to a Rust server.

This document is for other coding agents (and future humans) to quickly understand how the
project is structured, how state flows, and where to extend it.

## Repository Layout

- `server/`: Rust (Axum) HTTP + WebSocket server.
- `client/`: Rust -> WASM client (canvas rendering, tools, input handling).
- `shared/`: Types shared by server and client (stroke model + WS protocol).
- `public/`: Static files served by the server:
  - `index.html`, `styles.css`, `app.js`, icons under `public/icon/`
  - `public/pkg/`: output of `wasm-pack build client --target web --out-dir ../public/pkg`
- `sessions/`: on-disk server session snapshots (`.bin` from bincode; old `.json` may exist).
- `k8s/`, `Dockerfile`: deploy artifacts.

## Core Data Model

### Stroke

Defined in `shared/src/lib.rs`:

- `Stroke { id: StrokeId, color: Color, size: f32, points: Vec<Point> }`
- `StrokeId` is a random `[u64; 2]` (serde transparent).
- `Color { r: u8, g: u8, b: u8, a: u8 }` (parsed from hex in the client).
- `Point { x: f32, y: f32 }`

Important: points are in *world coordinates* (not canvas pixels). The client interprets world
coordinates under a `zoom/pan` transform. World coordinates are produced by
`client/src/dom.rs:event_to_point` using the inverse of the current pan/zoom.

### View Transform

The client maintains:

- `zoom: f64`
- `pan_x/pan_y: f64` (screen-space pixels; applied after scaling world space)
- `board_width/board_height: f64` (CSS pixels; canvas is device-pixel-ratio scaled)

Rendering uses the mapping:

- screen = world * zoom + pan

Line thickness is also scaled by zoom:

- `weight_px = stroke.size * zoom * STROKE_UNIT`

(`STROKE_UNIT` lives in `client/src/state.rs` and is a tuning constant.)

## Network Protocol (WebSocket)

Messages are encoded with `bincode` (v2) for the live websocket stream; the server still accepts
JSON text frames for debugging/backward-compatibility. All message types are defined in
`shared/src/lib.rs`.

### Session URL Scheme

- Visiting `/` creates a new session and redirects to `/s/:session_id`.
- WebSocket endpoint is `/ws/:session_id`.

The client computes the WS URL at runtime (`client/src/net.rs:websocket_url`) using:

- `wss://` when the page is served over `https:`
- `ws://` otherwise

### Server -> Client

- `sync { strokes }`: full state snapshot (sent on connect, and on `load`).
- `stroke:start`, `stroke:points`, `stroke:end`: incremental drawing.
- `stroke:remove`: delete a stroke by id.
- `stroke:restore`: restore a whole stroke (used for undo/redo + clear undo).
- `stroke:replace`: replace a stroke with the same id (used for transforms).
- `transform:update { ids, op }`: incremental transform updates (translate/scale/rotate deltas).
- `clear`: clear all strokes.

### Client -> Server

- `stroke:start`, `stroke:points`, `stroke:end`: draw a stroke.
- `erase { id }`: erase a stroke by id (eraser tool).
- `remove { ids }`: delete multiple strokes (selection delete/trash).
- `transform:update { ids, op }`: incremental transform updates (selection move/scale/rotate).
- `transform:start { ids }` / `transform:end { ids }`: brackets a transform so undo/redo treats it
  as one action.
- `clear`, `undo`, `redo`, `load { strokes }`

## Server Implementation

### Entry Point / Routing

`server/src/main.rs`:

- CLI args via `clap`:
  - `--sessions-dir`, `--public-dir`, `--backup-interval`, `--port`
  - TLS: `--tls-cert` and `--tls-key` (PEM; useful for mkcert / secure context testing)
- Serves:
  - `/` -> redirect to a new `/s/:uuid`
  - `/s/:uuid` -> serves `public/index.html` (single-page app)
  - `/ws/:uuid` -> websocket handler
  - everything else from `public/` via `ServeDir`
- Adds `Cache-Control/Pragma/Expires` headers to disable caching (helps iPad/Safari iteration).

### Session State / Consistency

`server/src/state.rs`:

- Global: `AppState.sessions: Arc<RwLock<HashMap<session_id, Arc<RwLock<Session>>>>>`
- Per-session: `Session` is behind a single `Arc<RwLock<Session>>` to keep updates consistent.

Key `Session` fields:

- `strokes: Vec<Stroke>`: canonical drawing state.
- `active_ids: HashSet<StrokeId>`: strokes currently being drawn (accept move/points only for these).
- `owners: HashMap<stroke_id, connection_uuid>`: who created a stroke (undo ownership isolation).
- `histories: HashMap<connection_uuid, ClientHistory>`: undo/redo stacks per connection.
- `transform_sessions: HashMap<connection_uuid, TransformSession>`: stores "before" snapshot for a
  transform grouping.
- `peers: HashMap<connection_uuid, mpsc::UnboundedSender<ServerMessage>>`: broadcast fanout.
- `dirty: bool`: flipped on any client message; used by periodic backups and on-last-peer exit.

### Apply + Broadcast

`server/src/handlers.rs`:

- On connect:
  - registers the peer
  - sends `ServerMessage::Sync { strokes }`
- On each inbound client message:
  - `apply_client_message(...)` mutates `Session`
  - returns `Vec<ServerMessage>` + a flag `include_sender` that controls broadcast:
    - `include_sender = false` -> broadcast to everyone except sender (sender already applied it)
    - `include_sender = true` -> broadcast to all (sender needs authoritative result too)

Broadcast avoids holding the session lock while sending to peers (prevents stalls/deadlocks).

### Undo/Redo Isolation

Undo/redo is owned by the server to ensure a client cannot undo other people’s work:

- `StrokeEnd` records an `Action::AddStroke` only for the stroke owner.
- Erase/Clear/Replace/Transform actions are pushed to the initiating connection’s history.
- `undo` and `redo` pop from the initiating connection’s history only.

Transform grouping:

- Client sends `transform:start { ids }` before first `transform:update` in a drag.
- Client sends `transform:end { ids }` on pointer-up/cancel.
- Server snapshots `before` at start and stores it in `transform_sessions`; at end it records a
  single `Action::Transform { before, after }`.

### Persistence / Backups

`server/src/sessions.rs`:

- Session snapshots are stored as `sessions_dir/{session_id}.bin`.
- Encoding is `bincode` of `Vec<Stroke>` (undo/redo buffers are *not* persisted).

Saving strategy:

- Periodic backup loop saves all `dirty` sessions every `--backup-interval` seconds.
- When the last peer disconnects, the server saves the session (if `dirty`) and removes it from
  memory.

## Client Implementation

### Entry Point / DOM Wiring

`public/app.js` loads `public/pkg/yumboard_client.js` and adds a `body.is-chrome` class for CSS.

`client/src/app.rs` (`#[wasm_bindgen(start)]`) wires:

- Canvas render context, toolbar buttons, slider, palette, status HUD.
- WebSocket connection + onmessage handlers (apply `ServerMessage` to local state).
- Pointer + keyboard handlers.

### Client State Machine

`client/src/state.rs`:

- `Mode` is the primary interaction mode:
  - `Draw(DrawState)`, `Erase(EraseMode)`, `Select(SelectState)`, `Pan(PanMode)`, `Loading(LoadingState)`
- Touch gestures are tracked separately from `Mode`:
  - `touch_points: HashMap<pointer_id, (x,y)>`
  - `pinch: Option<PinchState>`
  - `touch_pan: Option<PanMode>`

Design intent: finger gestures (touch) pan/pinch without mutating the primary mode (so a touch
pan does not permanently “steal” the pen tool).

### Rendering

`client/src/render.rs`:

- Draws strokes incrementally (for local input) and supports full redraw.
- Uses round caps and joins:
  - `ctx.set_line_cap("round")`, `ctx.set_line_join("round")`
- Selection overlay (when `Mode::Select`) draws:
  - dashed lasso polygon
  - selection bounding box
  - visible handles (corners + edges + rotate + trash)

### Tools / Gestures

#### Draw

- Pointer-down creates a stroke id and sends `stroke:start`.
- Pointer-move appends points locally and batches network sends:
  - points are buffered per-stroke and flushed in `requestAnimationFrame` via `stroke:points`
  - this is the main performance strategy (avoid WS message per pixel).

#### Erase

- While active, hit-tests strokes and removes them locally.
- Sends `erase { id }` per removed stroke (server broadcasts `stroke:remove`).

#### Select (Lasso + Handles)

- Lasso path is built on pointer-move and rendered as dashed line.
- On pointer-up, lasso selects *whole strokes* (by id) if *any point* of the stroke is inside the
  polygon (`client/src/actions.rs:finalize_lasso_selection`).
- Dragging handles emits `transform:update` ops in real time, so all peers see transforms live.
- Corner scaling keeps aspect ratio; edge scaling is axis-locked.
- Scaling is anchored at the opposite corner/side (selected handle determines anchor).

#### Pan / Zoom

- Pan tool: click-drag changes pan.
- Wheel scroll: zooms in/out around cursor position.
- Touch (finger):
  - 1-finger drag pans.
  - 2-finger pinch zooms around the pinch center.
  - Starting a pinch while drawing ends the current stroke to avoid mixed modes.

Home button recomputes a “fit to content” view using stroke bounds (`client/src/geometry.rs`).

### High-Frequency Input (Apple Pencil)

`client/src/app.rs`:

- Uses pointer capture on pointerdown (`setPointerCapture`) to reduce lost up/move events.
- Reads coalesced events when available:
  - `PointerEvent.getCoalescedEvents()` is called via JS `Reflect` to avoid binding issues.
  - Falls back to a single event if unsupported.
- Registers the same move handler for both `pointermove` and `pointerrawupdate` for higher-rate
  updates.

Note: `getCoalescedEvents` is only available in a secure context (HTTPS). The server supports TLS
flags to help local iPad testing.

### Save / Load

- Save JSON: creates a `data:` URL with `{ version: 1, strokes: [...] }` (`SaveData`).
- Save PDF: builds an SVG in an off-screen iframe and triggers `window.print()`. The SVG `viewBox`
  is set from content bounds so the whole drawing fits on one page.
- Load: reads a JSON file asynchronously (`FileReader`) and broadcasts `load { strokes }` after
  adopting it locally.
  - While reading, the mode becomes `Mode::Loading { previous: Mode, ... }`.

## Styling / Safari Notes

`public/styles.css` contains:

- Global `user-select: none` and `#board { touch-action: none; }`.
- A vertical size slider with a decorative “inverted triangle” behind it.
- Browser-specific behavior:
  - non-Chrome: disable animations and tool transitions (`body:not(.is-chrome)` rules)
  - iOS Safari: special slider layout via `@supports (-webkit-touch-callout: none)` and rotate.

The “hidden” color picker is an actual `<input type="color">` positioned on top of the currently
selected swatch (Safari requires it to be a real input to open the picker UI).

## Build / Run

Local (HTTP):

```sh
cargo install wasm-pack
wasm-pack build client --release --target web --out-dir ../public/pkg
cargo run --release -p yumboard_server -- --sessions-dir ./sessions --port 3000
```

Local (HTTPS / mkcert):

```sh
cargo run --release -p yumboard_server -- \
  --sessions-dir ./sessions \
  --public-dir ./public \
  --tls-cert ./keys/localhost+2.pem \
  --tls-key ./keys/localhost+2-key.pem \
  --port 3000
```

## Deployment Notes

- `Dockerfile` builds:
  - WASM client into `public/pkg/`
  - server binary `yumboard_server`
- `k8s/yumboard.yaml` contains a Deployment/Service/Ingress example and resource settings.
  - If you want session persistence, wire a real volume and pass `--sessions-dir` accordingly.

## Where To Implement New Features

- New protocol messages: `shared/src/lib.rs` (+ update both server/client handlers).
- Server-side semantics, validation, undo/redo rules: `server/src/logic.rs`.
- Session lifecycle / persistence: `server/src/handlers.rs`, `server/src/sessions.rs`.
- Client input routing/state machine: `client/src/app.rs`, `client/src/state.rs`.
- Geometry/transforms: `client/src/geometry.rs`.
- Rendering: `client/src/render.rs`.
- Save/load/PDF: `client/src/persistence.rs`.

## Known Limitations / Gotchas

- Session storage format is `bincode` of `Vec<Stroke>` and does not load old `.json` snapshots.
- Apple Pencil event delivery is browser-dependent; pointer capture + coalesced/raw events help,
  but “never drop an event” is not guaranteed by mobile browsers.
- iOS Safari layout is sensitive; the size slider has a dedicated code path in CSS.
