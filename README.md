![Tatara — create, sculpt, shade and orbit a ceramic collection in the real browser editor](docs/demo.gif)

# Tatara

**An AI-native 3D editor, built with Rust.**

Create in your browser. Give your agent the same tools. Keep every edit inspectable and undoable.

Tatara is an early, Blender-inspired project toward a complete 3D creation suite. Its first building block is a Rust modeling engine with a browser workspace and a stdio MCP bridge. Agents edit the actual scene you see, using the same commands as the UI.

> The GIF records the working editor's deterministic **tool workflow replay**. It shows real Rust mesh operations, not a generated video or an LLM response. The built-in replay needs no API key.

## Try it

Requirements: current stable Rust, Node.js 22+, npm and a browser with WebGL 2.

```sh
git clone https://github.com/rsasaki0109/Tatara.git
cd Tatara
npm --prefix web ci
npm --prefix web run build
cargo run --release
```

Open **http://127.0.0.1:3000** and click **Build a vessel study**, or start with a primitive.

The Rust process owns the scene. Save it as `.tatara.json` before closing the process; scene state is currently in memory. Use **Open** to restore a saved scene, or **Export OBJ** to use it elsewhere.

## What works today

- Browser workspace: studio shading, orbit/zoom, object selection, outliner, wireframe and transform gizmos.
- Rust geometry: cubes, spheres, cylinders, tori and hollow vessels revolved from a cross section.
- Modeling: edit transforms and materials, duplicate/rename/delete objects, select a face with **Alt + click** and extrude it.
- Undo/redo: an entire command batch is one history step. Invalid batches change nothing.
- Files: validated JSON scene save/load and OBJ export with object transforms applied.
- Agents: shared-scene MCP tools, generated command schemas, scene bounds and optional full mesh inspection.
- Optional AI chat: an OpenAI-compatible Chat Completions endpoint translates a request into validated commands.

Keyboard: **G** move, **R** rotate, **S** scale, **F** frame scene, **W** wireframe, **Delete** delete, **Ctrl/Cmd+Z** undo, **Ctrl/Cmd+Shift+Z** redo, **Ctrl/Cmd+S** save. Rotations in the inspector use radians. Y is up; scene units are meters.

## Connect your agent

Start the editor first. Add this to your MCP client's configuration, replacing the repository path:

```json
{
  "mcpServers": {
    "tatara": {
      "command": "cargo",
      "args": [
        "run", "--release",
        "--manifest-path", "/absolute/path/to/Tatara/Cargo.toml",
        "--", "--mcp"
      ]
    }
  }
}
```

Or build once and use the absolute path to `target/release/tatara` with `args: ["--mcp"]`.

| Tool | Purpose |
| --- | --- |
| `get_scene` | Read IDs, transforms, bounds, counts and revision. Set `include_mesh: true` for vertices and face indices. |
| `apply_commands` | Apply a validated, atomic modeling batch. Optional `expected_revision` rejects stale edits. |
| `undo` / `redo` | Reverse or restore the last batch in the shared scene. |

Example agent request:

> Create a hollow ceramic vase with a wide belly and a narrow neck. Give it a green glaze. Add a smaller terracotta version beside it.

The MCP bridge connects to `http://127.0.0.1:3000` by default. Set `TATARA_URL` in the MCP process if the editor uses a different port.

## Optional in-editor AI chat

MCP works with your existing agent; it does not require a provider key in Tatara. To enable natural-language chat inside the editor, configure an OpenAI-compatible provider **on the Rust server**:

```sh
export TATARA_AI_BASE_URL="http://127.0.0.1:11434/v1"
export TATARA_AI_MODEL="your-model-name"
# For a provider requiring authentication:
# export TATARA_AI_API_KEY="your-api-key"
cargo run --release
```

Reload the page. Chat uses `/chat/completions`, sends the prompt, current scene summary and command schema, and applies a JSON command batch. Choose a model that reliably returns JSON. Invalid responses and stale scene revisions are rejected; successful edits are undoable. Keys stay on the server. A configured remote provider receives the scene summary and prompt.

The provider path is covered with a mock provider in integration tests. Live hosted models have not been tested in this initial implementation.

## Command API

The UI, MCP bridge and chat share the same Rust engine. Inspect the generated schema at **`GET /api/schema`**. Paste a batch into the Agent workspace or call the local HTTP endpoint:

```sh
curl http://127.0.0.1:3000/api/commands \
  -H 'Content-Type: application/json' \
  -d '{"commands":[{"op":"add","name":"My cube","color":"#508f83","primitive":{"kind":"cube"}}]}'
```

Other routes: `GET /api/scene`, `GET /api/context`, `PUT /api/scene`, `POST /api/undo`, `POST /api/redo`, `GET /api/export/obj`. New IDs are allocated from the scene's `next_id`. Scene revisions are monotonic; passing `expected_revision` prevents overwriting concurrent edits.

## Architecture

| Part | Implementation |
| --- | --- |
| Geometry, scene, transforms, validation and history | Rust: `src/engine.rs` |
| Local server and optional provider integration | Rust: `src/server.rs` |
| Shared-scene stdio MCP bridge | Rust: `src/mcp.rs` |
| Browser controls and WebGL presentation | JavaScript + Three.js: `web/` |

The browser is a presentation layer; geometry generation and authoritative scene edits run in Rust. This first version uses a local Rust server, rather than a browser-only WASM runtime. It binds to loopback and is a single-user editor, with no accounts or hosted collaboration.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build
node scripts/check.mjs
npm --prefix web run build
npx --prefix web playwright install chromium
node scripts/browser-check.mjs
```

The browser check starts an isolated editor and verifies editing, history, save/open, OBJ download and responsive layout. `scripts/check.mjs` checks the real HTTP/MCP contract and optional chat using a mock provider. Do not point browser checks at unsaved work: they reset the scene.

To regenerate the README demo, install ffmpeg and run:

```sh
node scripts/record-demo.mjs
```

You can set `TATARA_BROWSER_PATH` to a Chromium executable. `TATARA_PORT` changes the server port; `TATARA_WEB_DIR` overrides the built asset directory when moving the binary.

## Direction

The goal is a full 3D creation suite with agents as first-class collaborators. Next: richer mesh editing and modifiers, glTF import/export, a browser-only Rust/WASM engine, then materials/nodes, animation and rendering.

This is an initial modeling prototype. It does not yet provide Blender feature parity, `.blend` compatibility, rigging, animation, UV editing or a production renderer. Face extrusion currently uses polygon normals, with no self-intersection repair. OBJ carries geometry; save the native scene to preserve materials and edit state.

## License

Dual-licensed under **MIT OR Apache-2.0**. This is an independent implementation; it does not embed Blender code.
