// Deterministic tool workflows used by "Play the tour" and by
// `node scripts/record-demo.mjs`, which turns each one into a README GIF.

const VASE = [[0.12, 0], [0.2, 0.06], [0.27, 0.28], [0.24, 0.5], [0.12, 0.72], [0.085, 0.82], [0.115, 0.9]]
const BOTTLE = [[0.1, 0], [0.17, 0.05], [0.19, 0.25], [0.15, 0.36], [0.055, 0.48], [0.045, 0.62], [0.062, 0.67]]
const CUP = [[0.065, 0], [0.085, 0.02], [0.1, 0.13], [0.108, 0.16]]
// Keyed neon colour cycle: pink, cyan, amber, back to pink.
const NEON = ['#ff4fd8', '#30e0ff', '#ffb02e', '#ff4fd8']

// The README's top GIF and the "Play the tour" button: sculpt, shade, let an
// agent build around it over MCP, then animate — about 25 seconds.
const TOUR_HEAD = { c: [0, 0.5, 0], r: 0.42 }
const tourFace = (x, y, out = 0) => {
  const { c, r } = TOUR_HEAD
  return [c[0] + x, c[1] + y, c[2] + Math.sqrt(Math.max(0, r * r - x * x - y * y)) + out]
}
export const hero = {
  title: 'Take the tour',
  width: 960,
  gizmo: false,
  external: true,
  setup: [
    { op: 'add', name: 'Plinth', primitive: { kind: 'cylinder', radius: 1.9, height: 0.08, segments: 96 }, translation: [0, 0.04, 0], color: '#26272b', roughness: 0.85 },
    { op: 'add', name: 'Head', primitive: { kind: 'quadsphere', radius: TOUR_HEAD.r, level: 5 }, translation: TOUR_HEAD.c, preset: 'clay', color: '#c98b62' },
  ],
  camera: { azimuth: 20, elevation: 6, distance: 2.6, target: [0, 0.52, 0] },
  steps: [
    { wait: 150 },
    { caption: 'Sculpt a clay ball into a character', hint: 'sculpt mode · mirror X' },
    { pick: 'Head', after: 60 },
    { click: '[data-mode=sculpt]', after: 80 },
    { slide: '#b-strength', to: 0.9, ms: 300, after: 40 },
    { stroke: [tourFace(0, 0.06), tourFace(0, -0.05), tourFace(0, -0.1)], ms: 500, after: 60 },
    { stroke: [tourFace(0.034, 0.143), tourFace(0.134, 0.168), tourFace(0.235, 0.118)], ms: 500, after: 60 },
    { slide: '#b-radius', to: 0.07, ms: 300, after: 40 },
    { stroke: [tourFace(0.1, 0.06), tourFace(0.143, 0.076), tourFace(0.168, 0.042), tourFace(0.134, 0.025), tourFace(0.11, 0.05)], ms: 500, ctrl: true, after: 60 },
    { stroke: [tourFace(0, -0.2), tourFace(0.07, -0.19), tourFace(0.13, -0.15)], ms: 450, ctrl: true, after: 60 },
    { click: '[data-brush=grab]', after: 40 },
    { slide: '#b-radius', to: 0.14, ms: 300, after: 40 },
    { camera: { azimuth: 46, elevation: 8 }, ms: 600 },
    { stroke: [tourFace(0.395, 0.017, -0.143), tourFace(0.54, 0.12, -0.143)], ms: 500, after: 80 },
    { click: '[data-mode=object]', after: 60 },
    { caption: 'Glass, metal or light in one click', hint: 'material presets' },
    { click: '[data-preset=gold]', after: 500 },
    { caption: 'Your agent builds in the same scene over MCP', hint: 'tatara --mcp' },
    { select: null },
    { camera: { azimuth: 10, elevation: 9, distance: 5.6, target: [0, 0.8, -0.25] }, ms: 1300, async: true },
    {
      mcp: [
        {
          tool: 'apply_commands',
          arguments: {
            commands: [
              { op: 'add', name: 'Vase', primitive: { kind: 'vessel', profile: VASE, thickness: 0.02, segments: 72 }, translation: [-1.15, 0.08, 0.15], scale: [1.3, 1.3, 1.3], preset: 'glass' },
              { op: 'add', name: 'Orb', primitive: { kind: 'sphere', radius: 0.28, segments: 48, rings: 24 }, translation: [1.1, 0.36, 0.2], preset: 'chrome' },
              { op: 'add', name: 'Ring', primitive: { kind: 'torus', major_radius: 1.25, minor_radius: 0.045, major_segments: 96, minor_segments: 16 }, translation: [0, 1.2, -0.95], rotation: [Math.PI / 2, 0, 0], preset: 'neon' },
            ],
          },
          after: 500,
        },
        { tool: 'render_view', arguments: { views: ['front', 'iso'], size: 256 }, after: 1000 },
      ],
    },
    { terminal: false },
    { caption: 'Key the glow and play it back', hint: 'keyframes · timeline' },
    {
      run: NEON.flatMap((c, i) => [
        { op: 'set_keyframe', id: 'Ring', property: 'color', frame: 1 + i * 31, value: c },
        { op: 'set_keyframe', id: 'Ring', property: 'emissive', frame: 1 + i * 31, value: c },
      ]),
      after: 200,
    },
    { click: '#play-btn', after: 200 },
    { cursor: false },
    { caption: 'UI, chat and MCP agents share one command API', hint: 'GET /api/schema' },
    { spin: 8 },
    { wait: 3600 },
  ],
}

// Face indices follow the engine's extrude rule: the extruded face keeps its
// index and the four new side faces are appended in edge order.
export const modeling = {
  title: 'Extrude and subdivide',
  width: 800,
  gizmo: false,
  camera: { azimuth: 12, elevation: 12, distance: 7.0, target: [0.15, 1.45, 0] },
  steps: [
    { wait: 150 },
    { caption: 'Add a cube', hint: 'add · cube' },
    { click: '[data-add=cube]', after: 200 },
    { caption: 'Alt+click a face, then extrude', hint: 'extrude · face 4' },
    { pick: 'Cube', face: 4 },
    { type: '0.55', into: '#p-dist', cps: 22 },
    { click: '#edit-group [data-action=extrude]' },
    { click: '#edit-group [data-action=extrude]' },
    { click: '#edit-group [data-action=extrude]', after: 100 },
    { caption: 'Grow arms from the side faces', hint: 'extrude · faces 7 → 20, 13 → 28' },
    { pick: 'Cube', face: 7 },
    { type: '0.4', into: '#p-dist', cps: 22 },
    { click: '#edit-group [data-action=extrude]' },
    { pick: 'Cube', face: 20 },
    { type: '0.6', into: '#p-dist', cps: 22 },
    { click: '#edit-group [data-action=extrude]' },
    { pick: 'Cube', face: 13 },
    { type: '0.4', into: '#p-dist', cps: 22 },
    { click: '#edit-group [data-action=extrude]' },
    { pick: 'Cube', face: 28 },
    { type: '0.45', into: '#p-dist', cps: 22 },
    { click: '#edit-group [data-action=extrude]', after: 150 },
    { caption: 'Catmull–Clark subdivision', hint: 'subdivide · 2 levels' },
    { pick: 'Cube' },
    { click: '#edit-group [data-action=subdivide]', after: 50 },
    { click: '#edit-group [data-action=subdivide]', after: 200 },
    { caption: 'Glaze it, inspect the quads', hint: 'material · wireframe' },
    { click: '[data-glaze^="#8fb9a0"]' },
    { click: '#wire-btn' },
    { cursor: false },
    { spin: 30 },
    { camera: { elevation: 9, distance: 6.4 }, ms: 1300 },
    { wait: 1000 },
  ],
}

const TEAPOT = [[0.12, 0], [0.2, 0.04], [0.245, 0.14], [0.225, 0.25], [0.13, 0.31], [0.115, 0.33]]
const TRAY_TOP = 0.04
const cups = [45, 135, 225, 315].map((deg, i) => {
  const a = (deg * Math.PI) / 180
  return {
    op: 'add',
    name: `Cup ${i + 1}`,
    primitive: { kind: 'vessel', profile: CUP, thickness: 0.014, segments: 56 },
    translation: [Math.round(Math.cos(a) * 0.5 * 1000) / 1000, TRAY_TOP, Math.round(Math.sin(a) * 0.5 * 1000) / 1000],
    color: '#ead9c6',
    roughness: 0.6,
  }
})

export const agent = {
  title: 'Agent chat (replay)',
  width: 800,
  tab: 'agent',
  camera: { azimuth: 20, elevation: 26, distance: 3.6, target: [0, 0.2, 0] },
  steps: [
    { wait: 150 },
    { caption: 'Ask in plain language', hint: 'chat → command batch' },
    {
      chat: 'Set a tea table: tenmoku teapot, four shino cups, celadon tray.',
      commands: [
        { op: 'add', name: 'Tray', primitive: { kind: 'cylinder', radius: 0.78, height: 0.04, segments: 72 }, translation: [0, 0.02, 0], color: '#8fb9a0', roughness: 0.25 },
        { op: 'add', name: 'Teapot', primitive: { kind: 'vessel', profile: TEAPOT, thickness: 0.015, segments: 64 }, translation: [0, TRAY_TOP, 0], color: '#3a2a22', roughness: 0.18 },
        { op: 'add', name: 'Spout', primitive: { kind: 'cylinder', radius: 0.042, radius_top: 0.018, height: 0.3, segments: 24 }, translation: [0.29, 0.25, 0], rotation: [0, 0, -0.95], color: '#3a2a22', roughness: 0.18 },
        { op: 'add', name: 'Handle', primitive: { kind: 'torus', major_radius: 0.1, minor_radius: 0.02, major_segments: 40, minor_segments: 12 }, translation: [-0.26, 0.21, 0], rotation: [Math.PI / 2, 0, 0], color: '#3a2a22', roughness: 0.18 },
        { op: 'add', name: 'Knob', primitive: { kind: 'sphere', radius: 0.035, segments: 24, rings: 12 }, translation: [0, 0.385, 0], color: '#b08d57', roughness: 0.3, metalness: 0.9 },
        ...cups,
      ],
      after: 300,
    },
    { caption: 'Validated, atomic and undoable', hint: 'revision 2' },
    { frame: { ms: 1000, padding: 1.0, elevation: 24 }, async: true },
    { spin: 10 },
    { wait: 600 },
    { caption: 'Refer to objects by name', hint: 'material · "Cup 1"…' },
    {
      chat: 'Make the cups copper red.',
      think: 450,
      commands: cups.map((c) => ({ op: 'material', id: c.name, color: '#9b2c2c', roughness: 0.16 })),
      after: 1800,
    },
  ],
}

const GLAZE_ROW = ['#8fb9a0', '#ead9c6', '#2f4f8f', '#b5643c', '#3a2a22', '#d8d4cb', '#9b2c2c']
const series = GLAZE_ROW.map((color, i) => {
  const t = i / (GLAZE_ROW.length - 1)
  const belly = 0.2 + 0.08 * Math.sin(t * Math.PI)
  const height = 0.55 + 0.35 * Math.cos(t * Math.PI * 1.3) ** 2
  const neck = 0.07 + 0.05 * t
  const profile = [
    [0.1, 0],
    [belly * 0.85, height * 0.12],
    [belly, height * 0.38],
    [neck + 0.02, height * 0.78],
    [neck, height * 0.9],
    [neck + 0.025, height],
  ].map(([r, y]) => [Math.round(r * 1000) / 1000, Math.round(y * 1000) / 1000])
  const a = (-60 + 120 * t) * (Math.PI / 180)
  return {
    op: 'add',
    name: `Vessel ${i + 1}`,
    primitive: { kind: 'vessel', profile, thickness: 0.018, segments: 64 },
    translation: [Math.round(Math.sin(a) * 1.5 * 1000) / 1000, 0, Math.round((-Math.cos(a) * 1.5 + 1.2) * 1000) / 1000],
    color,
    roughness: i % 2 ? 0.55 : 0.2,
  }
})

export const mcp = {
  title: 'MCP agent',
  width: 800,
  external: true,
  tab: 'agent',
  camera: { azimuth: 0, elevation: 22, distance: 6.2, target: [-0.35, 0.75, 0.1] },
  steps: [
    { wait: 200 },
    { caption: 'An external agent edits the same scene over MCP', hint: 'tatara --mcp' },
    {
      mcp: [
        { tool: 'get_scene', arguments: {}, after: 350 },
        { tool: 'apply_commands', arguments: { commands: series.slice(0, 4) }, after: 600 },
        { tool: 'apply_commands', arguments: { commands: series.slice(4) }, after: 600 },
        { tool: 'apply_commands', arguments: { commands: [{ op: 'clear' }], expected_revision: 0 }, after: 500 },
      ],
    },
    { caption: 'Stale edits are rejected; undo works for agents too', hint: 'expected_revision · undo' },
    {
      mcp: [
        { tool: 'undo', arguments: {}, after: 500 },
        { tool: 'redo', arguments: {}, after: 400 },
      ],
    },
    { terminal: false },
    { frame: { ms: 1200, padding: 1.2, elevation: 20 }, async: true },
    { spin: 9 },
    { wait: 2200 },
  ],
}

export const modifiers = {
  title: 'Modifier stack',
  width: 800,
  gizmo: false,
  camera: { azimuth: 30, elevation: 20, distance: 4.6, target: [0, 0.3, 0] },
  steps: [
    { wait: 150 },
    { caption: 'Flatten a cube into a slab', hint: 'transform · scale' },
    { click: '[data-add=cube]', after: 150 },
    { type: '0.3', into: '[data-field=scale][data-i="1"]', cps: 14, after: 150 },
    { caption: 'Inset the top face, then extrude it', hint: 'inset · extrude' },
    { pick: 'Cube', face: 4 },
    { click: '#edit-group [data-action=inset]', after: 100 },
    { type: '0.6', into: '#p-dist', cps: 18 },
    { click: '#edit-group [data-action=extrude]', after: 200 },
    { caption: 'Stack it with non-destructive modifiers', hint: 'array · twist · taper' },
    { click: '[data-add-mod=array]', after: 100 },
    { camera: { target: [0, 2.0, 0], distance: 11, elevation: 14 }, ms: 1500, async: true },
    { type: '9', into: '[data-mod="0"][data-field=count]', cps: 10, after: 250 },
    { click: '[data-add-mod=twist]', after: 100 },
    { slide: '[data-mod="1"][data-field=angle]', to: 270, after: 150 },
    { click: '[data-add-mod=taper]', after: 100 },
    { slide: '[data-mod="2"][data-field=factor]', to: 0.35, ms: 700, after: 150 },
    { caption: 'Smooth the whole stack', hint: 'subdivision' },
    { click: '[data-add-mod=subdivision]', after: 400 },
    { caption: 'Edit the base mesh — every layer follows', hint: 'extrude · face 4' },
    { pick: 'Cube', face: 4 },
    { click: '#edit-group [data-action=extrude]', after: 400 },
    { click: '[data-glaze^="#b5643c"]', after: 200 },
    { cursor: false },
    { spin: 26 },
    { camera: { elevation: 8, distance: 10 }, ms: 1500 },
    { wait: 800 },
  ],
}

// Vertex and face indices below follow the engine: a loop cut across the
// cube's top-front edge 7-6 adds vertices 8 (top front) and 11 (top back),
// and the front face becomes faces 0 and 1.
export const editing = {
  title: 'Edit mode',
  width: 800,
  gizmo: 'edit',
  camera: { azimuth: 32, elevation: 18, distance: 5.2, target: [0, 0.75, 0] },
  steps: [
    { wait: 150 },
    { caption: 'Tab into edit mode', hint: 'vertex · edge · face' },
    { click: '[data-add=cube]', after: 150 },
    { click: '[data-mode=edit]', after: 150 },
    { caption: 'Pick an edge, then loop cut across it', hint: 'loop_cut · Ctrl+R' },
    { click: '[data-select-mode=edge]' },
    { pick: 'Cube', edge: [7, 6], after: 150 },
    { click: '[data-action=loopCut]', after: 250 },
    { caption: 'Select the new ridge vertices and raise them', hint: 'move_vertices · G' },
    { click: '[data-select-mode=vertex]' },
    { pick: 'Cube', vertex: 8 },
    { pick: 'Cube', vertex: 11, shift: true, after: 200 },
    { run: [{ op: 'move_vertices', id: 'Cube', vertices: [8, 11], offset: [0, 0.6, 0] }], source: 'Gizmo', after: 300 },
    { caption: 'Inset and push in the front panels', hint: 'inset · extrude' },
    { click: '[data-select-mode=face]' },
    { pick: 'Cube', face: 0 },
    { pick: 'Cube', face: 1, shift: true },
    { type: '0.32', into: '#p-inset', cps: 20 },
    { click: '[data-action=inset]', after: 100 },
    { type: '-0.12', into: '#p-dist', cps: 20 },
    { click: '#edit-group [data-action=extrude]', after: 250 },
    { caption: 'Bevel every edge, then smooth', hint: 'bevel · subdivision' },
    { click: '[data-mode=object]', after: 100 },
    { type: '0.035', into: '#p-bevel', cps: 22 },
    { click: '[data-action=bevel]', after: 250 },
    { click: '[data-add-mod=subdivision]', after: 200 },
    { click: '[data-glaze^="#b5643c"]', after: 150 },
    { cursor: false },
    { select: null },
    { spin: 28 },
    { camera: { elevation: 12, distance: 4.6 }, ms: 1400 },
    { wait: 1000 },
  ],
}

// A scripted agent that uses render_view (a real `tatara --mcp` process)
// to notice its own mistake and fix it.
const VISION_VASE = [[0.11, 0], [0.19, 0.06], [0.25, 0.26], [0.22, 0.46], [0.11, 0.64], [0.08, 0.74], [0.105, 0.8]]
export const vision = {
  title: 'Agents can see',
  width: 800,
  external: true,
  tab: 'agent',
  camera: { azimuth: 10, elevation: 22, distance: 5.6, target: [-1.0, 0.3, 0] },
  steps: [
    { wait: 150 },
    { caption: 'The agent builds a still life over MCP', hint: 'apply_commands' },
    {
      mcp: [
        {
          tool: 'apply_commands',
          arguments: {
            commands: [
              { op: 'add', name: 'Plinth', primitive: { kind: 'cube' }, translation: [0, 0.04, 0], scale: [1.7, 0.08, 0.9], color: '#d8d4cb', roughness: 0.7 },
              { op: 'add', name: 'Vase', primitive: { kind: 'vessel', profile: VISION_VASE, thickness: 0.018, segments: 64 }, translation: [-0.25, 0.08, 0], color: '#2f4f8f', roughness: 0.2 },
              { op: 'add', name: 'Cup', primitive: { kind: 'vessel', profile: [[0.065, 0], [0.085, 0.02], [0.1, 0.13], [0.108, 0.16]], thickness: 0.014 }, translation: [0.02, 0.08, 0.1], color: '#b5643c', roughness: 0.7 },
            ],
          },
          after: 500,
        },
        { say: 'check the result before finishing', after: 200 },
        { tool: 'render_view', arguments: { views: ['front', 'top'], size: 256 }, after: 900 },
      ],
    },
    { caption: 'It looks at its work and spots the problem', hint: 'render_view' },
    {
      mcp: [
        { say: 'top view: Cup overlaps Vase. move it right, glaze it red', after: 300 },
        {
          tool: 'apply_commands',
          arguments: {
            commands: [
              { op: 'transform', id: 'Cup', translation: [0.45, 0.08, 0.12] },
              { op: 'material', id: 'Cup', color: '#9b2c2c', roughness: 0.16 },
            ],
          },
          after: 500,
        },
        { tool: 'render_view', arguments: { views: ['front', 'top'], size: 256 }, after: 400 },
        { say: 'no overlaps now. done', after: 600 },
      ],
    },
    { caption: 'Agents verify edits with their own eyes', hint: 'render_view · MCP' },
    { spin: 8 },
    { wait: 1800 },
  ],
}

// Keyframes through the real UI: key a pose, scrub, edit (auto-key), play.
export const animate = {
  title: 'Keyframe animation',
  width: 800,
  gizmo: false,
  setup: [
    { op: 'add', name: 'Plinth', primitive: { kind: 'cylinder', radius: 1.1, height: 0.08, segments: 64 }, translation: [0, 0.04, 0], color: '#d8d4cb', roughness: 0.7 },
  ],
  camera: { azimuth: 24, elevation: 14, distance: 6.2, target: [0, 1.0, 0] },
  steps: [
    { wait: 150 },
    { caption: 'Add a vessel and key its pose at frame 1', hint: 'set_keyframe · K' },
    { click: '[data-add=vessel]', after: 150 },
    { type: '0', into: '[data-field=translation][data-i="0"]', cps: 14 },
    { type: '0.08', into: '[data-field=translation][data-i="1"]', cps: 14, after: 100 },
    { click: '#key-btn', after: 250 },
    { caption: 'Scrub, then edit: animated values key themselves', hint: 'auto-key' },
    { scrub: 36, ms: 700 },
    { type: '1.3', into: '[data-field=translation][data-i="1"]', cps: 12 },
    { type: '-25', into: '[data-field=rotation][data-i="2"]', cps: 12, after: 100 },
    { scrub: 72, ms: 600 },
    { type: '0.08', into: '[data-field=translation][data-i="1"]', cps: 14 },
    { type: '0', into: '[data-field=rotation][data-i="2"]', cps: 12 },
    { click: '[data-glaze^="#2f4f8f"]', after: 250 },
    { caption: 'Play it back', hint: 'Space' },
    { cursor: false },
    { click: '#play-btn', after: 3400 },
    { caption: 'Exports as a glTF animation, and agents can render any frame', hint: 'GLB · render_view frame' },
    { wait: 1800 },
  ],
}

// Material presets through the real UI, then a keyed neon colour cycle.
export const materials = {
  title: 'Materials',
  width: 800,
  gizmo: false,
  setup: [
    { op: 'add', name: 'Plinth', primitive: { kind: 'cylinder', radius: 1.6, height: 0.08, segments: 72 }, translation: [0, 0.04, 0], color: '#3a3b40', roughness: 0.45 },
    { op: 'add', name: 'Vase', primitive: { kind: 'vessel', profile: VASE, thickness: 0.02, segments: 72 }, translation: [-0.78, 0.08, 0.15], scale: [1.35, 1.35, 1.35] },
    { op: 'add', name: 'Orb', primitive: { kind: 'sphere', radius: 0.3, segments: 48, rings: 24 }, translation: [0.02, 0.38, 0.45] },
    { op: 'add', name: 'Bottle', primitive: { kind: 'vessel', profile: BOTTLE, thickness: 0.018, segments: 72 }, translation: [0.8, 0.08, 0.1], scale: [1.3, 1.3, 1.3] },
    { op: 'add', name: 'Ring', primitive: { kind: 'torus', major_radius: 0.95, minor_radius: 0.04, major_segments: 96, minor_segments: 16 }, translation: [0, 1.05, -0.75], rotation: [Math.PI / 2, 0, 0] },
  ],
  camera: { azimuth: 6, elevation: 11, distance: 5.2, target: [0, 0.75, 0] },
  steps: [
    { wait: 150 },
    { caption: 'One click turns clay into glass, metal or light', hint: 'material · preset' },
    { click: '#outliner li:nth-child(2)', after: 120 },
    { click: '[data-preset=glass]', after: 350 },
    { click: '#outliner li:nth-child(3)', after: 120 },
    { click: '[data-preset=gold]', after: 350 },
    { click: '#outliner li:nth-child(4)', after: 120 },
    { click: '[data-preset=jade]', after: 350 },
    { click: '#outliner li:nth-child(5)', after: 120 },
    { click: '[data-preset=neon]', after: 450 },
    { caption: 'Turn up the glow', hint: 'emissive_strength' },
    { slide: '#p-emit', to: 4.5, ms: 800, after: 350 },
    { caption: 'Glow and colour are keyable', hint: 'set_keyframe · emissive' },
    {
      run: NEON.flatMap((c, i) => [
        { op: 'set_keyframe', id: 'Ring', property: 'color', frame: 1 + i * 31, value: c },
        { op: 'set_keyframe', id: 'Ring', property: 'emissive', frame: 1 + i * 31, value: c },
      ]),
      after: 300,
    },
    { cursor: false },
    { click: '#play-btn', after: 4200 },
    { caption: 'Exports to glTF with emission, transmission and alpha', hint: 'GLB · KHR_materials_*' },
    { spin: 7 },
    { wait: 2000 },
  ],
}

// Sculpting through the real UI: strokes are dragged over the surface (the
// points below are projected to the screen and ray-cast like a mouse).
const HEAD = { c: [0, 0.82, 0], r: 0.5 }
// A point on the front of the head, `x` right and `y` up from its centre.
const face = (x, y, out = 0) => {
  const z = Math.sqrt(Math.max(0, HEAD.r * HEAD.r - x * x - y * y)) + out
  return [HEAD.c[0] + x, HEAD.c[1] + y, HEAD.c[2] + z]
}
export const sculpt = {
  title: 'Sculpting',
  width: 800,
  gizmo: false,
  setup: [
    { op: 'add', name: 'Plinth', primitive: { kind: 'cylinder', radius: 0.55, height: 0.24, segments: 64 }, translation: [0, 0.12, 0], color: '#3a3b40', roughness: 0.45 },
    { op: 'add', name: 'Head', primitive: { kind: 'quadsphere', radius: HEAD.r, level: 5 }, translation: HEAD.c, preset: 'clay', color: '#c98b62' },
  ],
  camera: { azimuth: 22, elevation: 6, distance: 3.1, target: [0, 0.78, 0] },
  steps: [
    { wait: 150 },
    { caption: 'Sculpt mode: drag brushes over the surface', hint: 'sculpt · mirror X' },
    { pick: 'Head', after: 100 },
    { click: '[data-mode=sculpt]', after: 150 },
    { slide: '#b-strength', to: 0.85, ms: 400, after: 100 },
    { stroke: [face(0, 0.07), face(0, -0.06), face(0, -0.12)], ms: 700, after: 100 },
    { stroke: [face(0.04, 0.17), face(0.16, 0.2), face(0.28, 0.14)], ms: 700, after: 120 },
    { caption: 'Ctrl carves; a smaller radius adds detail', hint: 'invert · radius' },
    { slide: '#b-radius', to: 0.08, ms: 450, after: 80 },
    { stroke: [face(0.12, 0.07), face(0.17, 0.09), face(0.2, 0.05), face(0.16, 0.03), face(0.13, 0.06)], ms: 700, ctrl: true, after: 100 },
    { stroke: [face(0, -0.24), face(0.08, -0.23), face(0.15, -0.18)], ms: 600, ctrl: true, after: 120 },
    { caption: 'Grab pulls out ears; Inflate swells the cheeks', hint: 'grab · inflate' },
    { click: '[data-brush=grab]', after: 60 },
    { slide: '#b-radius', to: 0.16, ms: 400, after: 60 },
    { camera: { azimuth: 48, elevation: 8 }, ms: 700 },
    { stroke: [face(0.47, 0.02, -0.17), face(0.64, 0.14, -0.17)], ms: 600, after: 100 },
    { click: '[data-brush=inflate]', after: 60 },
    { stroke: [face(0.2, -0.09), face(0.25, -0.14)], ms: 500, after: 120 },
    { caption: 'Shift smooths; every stroke is one undoable command', hint: 'smooth · undo' },
    { stroke: [face(-0.22, -0.04), face(0, 0.02), face(0.22, -0.04)], ms: 700, shift: true, after: 120 },
    { click: '[data-mode=object]', after: 100 },
    { cursor: false },
    { caption: 'Agents sculpt with the same command', hint: '{"op": "sculpt", "brush": "draw", ...}' },
    { camera: { azimuth: 24, elevation: 6 }, ms: 800, async: true },
    { spin: 9 },
    { wait: 2600 },
  ],
}

// An agent measures its own work: a render looks fine, but inspect_scene
// finds a sunk vase, an apple through a book and a floating orb; `drop`
// settles all three and a second inspection comes back clean.
export const inspect = {
  title: 'Agents measure their work',
  width: 800,
  external: true,
  tab: 'agent',
  gizmo: false,
  camera: { azimuth: 12, elevation: 20, distance: 6.2, target: [-1.1, 0.3, 0] },
  steps: [
    { wait: 150 },
    { caption: 'The agent builds a still life over MCP', hint: 'apply_commands' },
    {
      mcp: [
        {
          tool: 'apply_commands',
          arguments: {
            commands: [
              { op: 'add', name: 'Plinth', primitive: { kind: 'cube' }, translation: [0, 0.05, 0], scale: [2.4, 0.1, 1.2], color: '#d8d4cb', roughness: 0.7 },
              { op: 'add', name: 'Vase', primitive: { kind: 'vessel', profile: VASE, thickness: 0.02, segments: 64 }, translation: [-0.6, 0.02, 0], color: '#2f4f8f', roughness: 0.2 },
              { op: 'add', name: 'Book', primitive: { kind: 'cube' }, translation: [0.15, 0.14, 0.05], rotation: [0, 0.3, 0], scale: [0.5, 0.08, 0.35], color: '#9b2c2c', roughness: 0.6 },
              { op: 'add', name: 'Apple', primitive: { kind: 'sphere', radius: 0.12, segments: 32, rings: 16 }, translation: [0.2, 0.17, 0.05], color: '#7cb342', roughness: 0.35 },
              { op: 'add', name: 'Orb', primitive: { kind: 'sphere', radius: 0.16, segments: 48, rings: 24 }, translation: [0.8, 0.5, -0.1], preset: 'chrome' },
            ],
          },
          after: 400,
        },
        { tool: 'render_view', arguments: { views: ['front'], size: 256 }, after: 500 },
        { say: 'looks right. measure it before finishing', after: 200 },
      ],
    },
    { caption: 'inspect_scene measures what a picture hides', hint: 'inspect_scene' },
    { mcp: [{ tool: 'inspect_scene', after: 1600 }], clear: true },
    { caption: 'One command settles each object on what is below it', hint: 'drop' },
    {
      clear: true,
      mcp: [
        { say: 'drop the vase, apple and orb onto their supports', after: 200 },
        { tool: 'apply_commands', arguments: { commands: [{ op: 'drop', id: 'Vase' }, { op: 'drop', id: 'Apple' }, { op: 'drop', id: 'Orb' }] }, after: 500 },
        { tool: 'inspect_scene', after: 600 },
        { say: 'no issues. done', after: 500 },
      ],
    },
    { caption: 'Measured, fixed and verified by the agent', hint: 'inspect_scene · drop' },
    { spin: 8 },
    { wait: 2000 },
  ],
}

// An agent furnishes a room by relation, not coordinates: build, arrange
// around, place on and beside, then verify with inspect_scene.
export const furnish = {
  title: 'Agents describe layouts, not coordinates',
  width: 800,
  external: true,
  gizmo: false,
  setup: [{ op: 'add', name: 'Rug', primitive: { kind: 'cube' }, translation: [0, 0.005, 0], scale: [3.4, 0.01, 2.6], color: '#cfc6b8', roughness: 0.95 }],
  camera: { azimuth: 28, elevation: 24, distance: 7.2, target: [-1.0, 0.45, 0] },
  steps: [
    { wait: 150 },
    { caption: 'Agents describe layouts, not coordinates', hint: 'build · arrange · place' },
    {
      mcp: [
        { say: 'a dining table with four chairs around it', after: 150 },
        {
          tool: 'apply_commands',
          arguments: {
            commands: [
              { op: 'build', template: 'table' },
              ...[1, 2, 3, 4].map(() => ({ op: 'build', template: 'chair' })),
              { op: 'arrange', ids: ['Chair', 'Chair 2', 'Chair 3', 'Chair 4'], layout: 'circle', around: 'Table' },
            ],
          },
          after: 700,
        },
        { say: 'a lamp and a mug on the table', after: 150 },
        {
          tool: 'apply_commands',
          arguments: {
            commands: [
              { op: 'build', template: 'lamp' },
              { op: 'place', id: 'Lamp', on: 'Table', at: [0.85, 0.3] },
              { op: 'build', template: 'mug' },
              { op: 'place', id: 'Mug', on: 'Table', at: [0.35, 0.65] },
            ],
          },
          after: 600,
        },
        { say: 'a plant beside it, a shelf behind', after: 150 },
        {
          tool: 'apply_commands',
          arguments: {
            commands: [
              { op: 'build', template: 'plant' },
              { op: 'place', id: 'Plant', beside: 'Table', side: 'right', gap: 0.85 },
              { op: 'build', template: 'shelf', color: '#6f4a33' },
              { op: 'place', id: 'Shelf', beside: 'Table', side: 'back', gap: 0.8 },
            ],
          },
          after: 600,
        },
      ],
    },
    { caption: 'Then it checks the result', hint: 'inspect_scene' },
    { mcp: [{ tool: 'inspect_scene', after: 900 }] },
    { terminal: false },
    { caption: 'Each piece stays one group: move, turn or drop it as a unit', hint: 'groups' },
    { camera: { azimuth: 36, elevation: 22, distance: 6.0, target: [0.1, 0.45, 0] }, ms: 1000, async: true },
    { click: '#outliner li[data-group="Lamp"]', after: 900 },
    { cursor: false },
    { select: null },
    { spin: 8 },
    { wait: 2200 },
  ],
}

// Booleans through the real UI, then an agent drills the pips of a die.
const DIE = [2.0, 0.5, 0]
const pip = (k, x, y, z) => [
  { op: 'add', name: `Pip ${k}`, primitive: { kind: 'sphere', radius: 0.085, segments: 20, rings: 10 }, translation: [DIE[0] + x, DIE[1] + y, DIE[2] + z] },
  { op: 'boolean', id: 'Die', with: `Pip ${k}`, operation: 'difference' },
]
export const boolean = {
  title: 'Booleans',
  width: 800,
  external: true,
  gizmo: false,
  setup: [
    { op: 'add', name: 'Block', primitive: { kind: 'cube' }, translation: [-0.4, 0.45, 0], scale: [1.3, 0.9, 0.9], color: '#8fb9a0', roughness: 0.3 },
    { op: 'add', name: 'Drill', primitive: { kind: 'cylinder', radius: 0.24, height: 1.6, segments: 48 }, translation: [-0.4, 0.45, 0], rotation: [Math.PI / 2, 0, 0], color: '#ff7a3d', roughness: 0.4, opacity: 0.55 },
    { op: 'add', name: 'Die', primitive: { kind: 'cube' }, translation: DIE, color: '#f4f1ea', roughness: 0.22 },
    { op: 'add', name: 'Ball', primitive: { kind: 'sphere', radius: 0.68, segments: 48, rings: 24 }, translation: DIE, color: '#ff7a3d', roughness: 0.4, opacity: 0.45 },
  ],
  camera: { azimuth: 24, elevation: 20, distance: 5.6, target: [0.8, 0.45, 0] },
  steps: [
    { wait: 150 },
    { caption: 'Cut one shape out of another', hint: 'boolean · difference' },
    { pick: 'Block', after: 100 },
    { choose: '#bool-with', value: 'Drill', after: 200 },
    { click: '[data-bool=difference]', after: 700 },
    { caption: 'Keep only the overlap: a cube and a sphere make a die', hint: 'boolean · intersect' },
    { pick: 'Die', after: 100 },
    { choose: '#bool-with', value: 'Ball', after: 200 },
    { click: '[data-bool=intersect]', after: 700 },
    { caption: 'An agent drills the pips with the same command', hint: '{"op": "boolean", "operation": "difference"}' },
    { select: null },
    {
      mcp: [
        { say: 'six pips: one on top, two on the right, three in front', after: 150 },
        {
          tool: 'apply_commands',
          arguments: {
            commands: [
              ...pip(1, 0, 0.5, 0),
              ...pip(2, 0.5, 0.22, -0.22),
              ...pip(3, 0.5, -0.22, 0.22),
              ...pip(4, -0.25, 0.25, 0.5),
              ...pip(5, 0, 0, 0.5),
              ...pip(6, 0.25, -0.25, 0.5),
            ],
          },
          after: 600,
        },
      ],
    },
    { terminal: false },
    { cursor: false },
    { caption: 'Every cut is one undoable step', hint: 'boolean · undo' },
    { spin: 9 },
    { wait: 2600 },
  ],
}

// Procedural textures through the real UI: presets, pattern chips and the
// tile-size slider, on a room corner with furniture from `build`.
export const textures = {
  title: 'Textures',
  width: 800,
  gizmo: false,
  setup: [
    { op: 'add', name: 'Floor', primitive: { kind: 'cube' }, translation: [0, -0.01, 0], scale: [3.4, 0.02, 2.6], color: '#d9d4cb', roughness: 0.6 },
    { op: 'add', name: 'Wall', primitive: { kind: 'cube' }, translation: [0, 0.8, -1.3], scale: [3.4, 1.6, 0.08], color: '#c8c2b8', roughness: 0.9 },
    { op: 'build', template: 'table' },
    { op: 'add', name: 'Vase', primitive: { kind: 'vessel', profile: VASE, thickness: 0.02, segments: 64 }, translation: [0.25, 0.75, 0], scale: [0.6, 0.6, 0.6], color: '#e8e4dc', roughness: 0.3 },
    { op: 'add', name: 'Crate', primitive: { kind: 'cube' }, translation: [-1.15, 0.25, 0.55], rotation: [0, 0.35, 0], scale: [0.5, 0.5, 0.5], color: '#b88a5a', roughness: 0.7 },
  ],
  camera: { azimuth: 22, elevation: 16, distance: 5.2, target: [-0.1, 0.6, 0] },
  steps: [
    { wait: 150 },
    { caption: 'Procedural textures: no UV unwrapping', hint: 'material · preset' },
    { pick: 'Floor', after: 100 },
    { click: '[data-preset=tiles]', after: 450 },
    { pick: 'Wall', after: 100 },
    { click: '[data-preset=brick]', after: 450 },
    { caption: 'Tile size is in metres', hint: 'texture.scale' },
    { slide: '#p-tscale', to: 0.45, ms: 800, after: 350 },
    { pick: 'Vase', after: 100 },
    { click: '[data-preset=marble]', after: 450 },
    { caption: 'Any colour, any pattern', hint: 'texture.pattern · color2' },
    { pick: 'Crate', after: 100 },
    { click: '[data-pattern=wood]', after: 450 },
    { click: '[data-pattern=checker]', after: 450 },
    { click: '[data-pattern=wood]', after: 400 },
    { select: null },
    { cursor: false },
    { caption: 'Baked into glTF, the same in any viewer', hint: 'GLB · baseColorTexture' },
    { spin: 7 },
    { wait: 2200 },
  ],
}

export const SCENARIOS = { hero, modeling, agent, mcp, modifiers, editing, vision, animate, materials, sculpt, inspect, furnish, boolean, textures }
