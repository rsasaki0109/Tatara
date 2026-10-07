<p align="center">
  <img src="docs/media/hero.gif" width="960" alt="Tatara building a ceramic vessel study in the browser editor: revolve a profile, glaze it, add a bowl, bottle and cups, undo and redo, then orbit the scene">
</p>

<h1 align="center">Tatara</h1>

<p align="center"><b>An AI-native 3D editor, built with Rust.</b><br>
Create in your browser. Give your agent the same tools. Keep every edit inspectable and undoable.</p>

<table>
  <tr>
    <td width="50%"><img src="docs/media/editing.gif" alt="In edit mode a cube gets a loop cut, its ridge vertices are raised into a gable roof, two front panels are inset and pushed in, then every edge is bevelled and smoothed"></td>
    <td width="50%"><img src="docs/media/modeling.gif" alt="A cube is extruded face by face into a cactus, subdivided with Catmull-Clark, glazed and shown in wireframe"></td>
  </tr>
  <tr>
    <td><b>Edit</b> — vertex, edge and face selection, loop cut, move and bevel.</td>
    <td><b>Model</b> — Alt+click a face, extrude, subdivide, glaze.</td>
  </tr>
  <tr>
    <td width="50%"><img src="docs/media/modifiers.gif" alt="A slab is inset and extruded, then stacked with array, twist, taper and subdivision modifiers; extruding the base face updates every layer"></td>
    <td width="50%"><img src="docs/media/agent.gif" alt="A chat request becomes a validated command batch that builds a tea set; a second request recolors the cups by name"></td>
  </tr>
  <tr>
    <td><b>Stack</b> — non-destructive modifiers; edit the base and every layer follows.</td>
    <td><b>Ask</b> — chat turns into an atomic, undoable command batch.</td>
  </tr>
  <tr>
    <td colspan="2"><img src="docs/media/mcp.gif" width="50%" alt="An external agent calls tatara --mcp tools; a stale edit is rejected and undo/redo work from the agent"><br><b>Connect</b> — any MCP agent edits the scene you are looking at.</td>
  </tr>
</table>

Every GIF above is a deterministic recording of the real editor: real toolbar clicks and face picks, real Rust mesh operations and a real `tatara --mcp` process. The chat clip replays a fixed command batch, so it needs no API key. Regenerate them all with `node scripts/record-demo.mjs`.

## Why Tatara

Blender is the benchmark for what a 3D suite can do. Tatara starts from a different premise: **agents are first-class users**. Every action, from a toolbar click to a chat request or an MCP call, becomes the same typed command. That gives you:

- **One command API for everything.** The UI, the in-editor chat and external agents all call `POST /api/commands`, described by a generated JSON Schema at `GET /api/schema`.
- **Edits you can trust.** A batch is validated and applied atomically: if any command fails, nothing changes. Each batch is one undo step, and `expected_revision` rejects edits based on a stale scene.
- **A shared, live scene.** Agents edit the scene open in your browser, and the change appears immediately.
- **Reproducible output.** Scenes are plain JSON, geometry is generated in Rust, and demo workflows replay frame-for-frame.

It is early. Today it is a modeling prototype, and the [roadmap](#roadmap) shows the way toward a full creation suite.

## Try it

Requirements: current stable Rust, Node.js 22+, and a browser with WebGL 2.

```sh
git clone https://github.com/rsasaki0109/Tatara.git
cd Tatara
npm --prefix web ci
npm --prefix web run build
cargo run --release
```

Open **http://127.0.0.1:3000** and click **Build a vessel study**, or add a primitive from the toolbar.

The Rust process owns the scene and keeps it in memory. Use **Save** to download a `.tatara.json` file, **Open** to restore one and **OBJ** to export geometry.

## What works today

- **Workspace:** studio lighting with shadows, orbit and zoom, outliner, properties panel, transform gizmo, polygon wireframe, axis widget and a phone layout.
- **Geometry (Rust):** cube, plane, UV sphere, cylinder or cone, torus, and hollow **vessels** revolved from a smoothed cross section.
- **Modeling:** transform, glaze presets and PBR material, rename, duplicate, array, delete, per-face inset and extrusion, and Catmull-Clark subdivision.
- **Edit mode (Tab):** vertex, edge and face selection with Shift+click and select all; move a selection with the gizmo; loop cut; bevel selected edges or every edge; extrude and inset several faces at once.
- **Modifier stack:** non-destructive Mirror, Subdivision, Array, Twist and Taper, evaluated in Rust in order. The base mesh stays editable and is drawn as an orange cage. **Apply** bakes the stack into the base mesh.
- **History:** each batch is one undo step, and an invalid batch changes nothing.
- **Files:** validated JSON scene save/open, plus OBJ export with transforms applied.
- **Agents:** a stdio MCP bridge, generated command schemas, scene bounds and optional full mesh reads.
- **Chat (optional):** an OpenAI-compatible Chat Completions endpoint translates requests into validated commands.

| Key | Action | Key | Action |
| --- | --- | --- | --- |
| Alt + click | Select a face (on the base cage) | E / I | Extrude / inset selected face |
| G / R / S | Move / rotate / scale gizmo | Shift + D | Duplicate |
| F | Frame scene | X / Delete | Delete |
| W | Wireframe | Ctrl/Cmd + Z | Undo (add Shift to redo) |
| Tab | Edit mode | 1 / 2 / 3 | Vertex / edge / face select (edit mode) |
| A | Select all (edit mode) | Ctrl + B / Ctrl + R | Bevel / loop cut |
| Esc | Leave edit mode or deselect | Ctrl/Cmd + S | Save |

Units are meters and Y is up. The inspector shows rotations in degrees; the command API takes radians.

## Connect your agent

Start the editor first, then add Tatara to your MCP client's configuration:

```json
{
  "mcpServers": {
    "tatara": {
      "command": "cargo",
      "args": ["run", "--release", "--manifest-path", "/absolute/path/to/Tatara/Cargo.toml", "--", "--mcp"]
    }
  }
}
```

You can also build once and point the client at `target/release/tatara` with `args: ["--mcp"]`.

| Tool | Purpose |
| --- | --- |
| `get_scene` | IDs, names, transforms, materials, world bounds, counts and revision. Pass `include_mesh: true` for vertices and polygons. |
| `apply_commands` | Apply a validated, atomic batch. Pass `expected_revision` to reject stale edits. |
| `undo` / `redo` | Step the shared history. |

Example request for your agent:

> Make a hollow celadon vase with a wide belly and a narrow neck, and a smaller terracotta one beside it.

The bridge connects to `http://127.0.0.1:3000`. Set `TATARA_URL` if the editor runs elsewhere.

## Command API

```sh
curl http://127.0.0.1:3000/api/commands \
  -H 'Content-Type: application/json' \
  -d '{"commands":[
        {"op":"add","name":"Vase","primitive":{"kind":"vessel","profile":[[0.12,0],[0.27,0.28],[0.12,0.72],[0.11,0.9]]},"color":"#8fb9a0","roughness":0.22},
        {"op":"duplicate","id":"Vase","offset":[0.8,0,0]}
      ]}'
```

| Op | Fields |
| --- | --- |
| `add` | `primitive` (`cube`, `plane`, `sphere`, `cylinder`, `torus`, `vessel`), optional `name`, `translation`, `rotation`, `scale`, `color`, `roughness`, `metalness` |
| `transform` / `material` / `rename` | `id`, plus the fields to change |
| `duplicate` / `array` | `id`, `offset` (and `count` for `array`) |
| `extrude` / `inset` | `id`, `face`, and `distance` or `fraction` (0–1) |
| `move_vertices` | `id`, `vertices` (indices), `offset` |
| `bevel` | `id`, `width`, optional `edges` as `[[a, b], …]` (all edges when omitted) |
| `loop_cut` | `id`, `edge` `[a, b]`, optional `fraction` |
| `add_modifier` / `set_modifier` / `remove_modifier` | `id`, `modifier` (`mirror`, `subdivision`, `array`, `twist`, `taper`), `index` |
| `apply_modifiers` | `id`: bake the stack into the base mesh |
| `subdivide` | `id`, `levels` (1–4) |
| `delete` / `clear` | `id` / nothing |

`id` accepts a numeric ID or an exact object name. New objects take IDs from the scene's `next_id`, in order. Face indices refer to the base mesh. An extruded or inset face keeps its index, and its four new side faces are appended in edge order, so an agent can chain edits without re-reading the mesh. `get_scene` reports both the base and evaluated face counts, and bounds come from the evaluated mesh.

```json
{"commands": [
  {"op": "add", "name": "Tower", "primitive": {"kind": "cube"}, "scale": [1, 0.3, 1]},
  {"op": "add_modifier", "id": "Tower", "modifier": {"type": "array", "count": 9, "offset": [0, 1.2, 0]}},
  {"op": "add_modifier", "id": "Tower", "modifier": {"type": "twist", "angle": 4.7}},
  {"op": "add_modifier", "id": "Tower", "modifier": {"type": "taper", "factor": 0.35}},
  {"op": "add_modifier", "id": "Tower", "modifier": {"type": "subdivision", "levels": 1}}
]}
```

Other routes are `GET /api/scene`, `GET /api/context`, `GET /api/state`, `PUT /api/scene`, `POST /api/undo`, `POST /api/redo`, `GET /api/export/obj` and `GET /api/events`. The last is a server-sent event stream of revisions.

## Optional in-editor chat

Configure an OpenAI-compatible provider on the **server**. The key never reaches the browser.

```sh
export TATARA_AI_BASE_URL="http://127.0.0.1:11434/v1"
export TATARA_AI_MODEL="your-model-name"
# export TATARA_AI_API_KEY="..."   # if the provider needs one
cargo run --release
```

Chat sends your prompt, a scene summary and the command schema to `/chat/completions`. It applies the returned JSON batch at the revision the prompt saw. Invalid or stale replies are rejected, and successful ones are undoable. Integration tests cover this path with a mock provider; it has not been tested against live hosted models.

## README GIFs

The GIFs come from scripted workflows in [`web/src/scenarios.js`](web/src/scenarios.js). They are rendered with the following pipeline:

1. `scripts/record-demo.mjs` starts a fresh `tatara` server for each scenario and opens the editor in headless Chromium with `?capture=1`.
2. In capture mode the editor runs on a **virtual clock**. The recorder advances it one frame at a time and waits for every API call, then takes a screenshot. Slow software rendering therefore never drops a frame, and every run produces the same GIF.
3. A scenario drives the editor through the real UI: a cursor clicks toolbar buttons and glaze chips, picks faces in the viewport and types into fields. Captions explain each step, and MCP scenarios talk to a real `tatara --mcp` child process.
4. ffmpeg encodes the frames with a two-pass palette into `docs/media/<scenario>.gif`.

```sh
node scripts/record-demo.mjs                # every scenario
node scripts/record-demo.mjs hero modeling  # selected ones
node scripts/record-demo.mjs --fps 12 --mp4 # lower frame rate, also write MP4
```

This needs ffmpeg and a Chromium build. Set `TATARA_BROWSER_PATH` if Playwright's own browser is not installed. To add a GIF, add a scenario to `SCENARIOS`; the recorder picks it up automatically. The **Build a vessel study** button plays the `hero` scenario live.

## Architecture

| Part | Implementation |
| --- | --- |
| Geometry, commands, validation, history, OBJ | Rust: `src/engine.rs` |
| Modifier stack evaluation | Rust: `src/modifiers.rs` |
| Bevel, loop cut, vertex moves | Rust: `src/edit.rs` |
| HTTP API, live events, chat provider | Rust: `src/server.rs` (axum) |
| Stdio MCP bridge | Rust: `src/mcp.rs` |
| Editor UI and WebGL presentation | JavaScript + Three.js: `web/src` |
| Scripted workflows, recorder, smoke test | `web/src/scenarios.js`, `scripts/` |

The browser is a presentation layer; geometry and authoritative edits run in Rust. The server binds to loopback and is single-user.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                       # engine, HTTP API, MCP bridge, mock chat
npm --prefix web ci && npm --prefix web run build
cargo build --release
node scripts/browser-check.mjs   # real-browser editing, history, files, layout and all scenarios
```

`TATARA_PORT` changes the port, and `TATARA_WEB_DIR` points at the built assets when you move the binary. The browser check starts its own isolated server.

## Roadmap

The aim is a creation suite that surpasses Blender for human and agent collaboration, taken one verifiable step at a time:

1. **Modeling depth:** ~~modifier stack~~ ✓, ~~inset~~ ✓, ~~edit mode, bevel, loop cut~~ ✓; next: multi-segment bevel, knife, merge and dissolve, booleans, and solidify/bevel/boolean modifiers.
2. **Interchange:** glTF import and export, and a browser-only Rust/WASM engine.
3. **Look development:** a node-based material system, UVs and textures, and a path-traced preview.
4. **Motion:** keyframes, curves, constraints, then rigging.
5. **Agents:** visual feedback for agents (renders and measurements as tool results), reviewable change proposals and multi-user sessions.

Not yet available: `.blend` compatibility, UV editing, animation and a production renderer. Extrusion moves a face along its normal and does not repair self-intersections. OBJ carries geometry only; save the native scene to keep materials.

## License

Dual-licensed under **MIT OR Apache-2.0**. Tatara is an independent implementation and contains no Blender code.
