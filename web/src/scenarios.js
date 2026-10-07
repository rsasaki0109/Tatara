// Deterministic tool workflows used by "Build a vessel study" and by
// `node scripts/record-demo.mjs`, which turns each one into a README GIF.

const BOARD = {
  op: 'add',
  name: 'Board',
  primitive: { kind: 'cube' },
  translation: [0, 0.03, 0],
  scale: [3.0, 0.06, 1.3],
  color: '#6b5443',
  roughness: 0.82,
}
const Y = 0.06

const VASE = [[0.12, 0], [0.2, 0.06], [0.27, 0.28], [0.24, 0.5], [0.12, 0.72], [0.085, 0.82], [0.115, 0.9]]
const BOWL = [[0.1, 0], [0.13, 0.025], [0.29, 0.13], [0.37, 0.23]]
const BOTTLE = [[0.1, 0], [0.17, 0.05], [0.19, 0.25], [0.15, 0.36], [0.055, 0.48], [0.045, 0.62], [0.062, 0.67]]
const CUP = [[0.065, 0], [0.085, 0.02], [0.1, 0.13], [0.108, 0.16]]

export const hero = {
  title: 'Build a vessel study',
  width: 960,
  gizmo: false,
  setup: [BOARD],
  camera: { azimuth: 18, elevation: 16, distance: 3.3, target: [-0.55, 0.38, 0] },
  steps: [
    { spin: 4 },
    { wait: 300 },
    { caption: 'Revolve a cross section into a hollow vessel', hint: 'add · vessel' },
    {
      run: [{ op: 'add', name: 'Vase', primitive: { kind: 'vessel', profile: VASE, thickness: 0.02, segments: 72 }, translation: [-1.0, Y, -0.12] }],
      select: 'created',
      after: 500,
    },
    { caption: 'Glaze it', hint: 'material · celadon' },
    { click: '[data-glaze^="#8fb9a0"]', after: 500 },
    { caption: 'Build out the collection', hint: 'add · duplicate' },
    { camera: { target: [0, 0.35, 0], distance: 4.9, elevation: 19 }, ms: 1500, async: true },
    {
      run: [
        { op: 'add', name: 'Bowl', primitive: { kind: 'vessel', profile: BOWL, thickness: 0.018, segments: 72 }, translation: [0.12, Y, 0.12], color: '#3a2a22', roughness: 0.18 },
        { op: 'add', name: 'Bottle', primitive: { kind: 'vessel', profile: BOTTLE, thickness: 0.018, segments: 72 }, translation: [1.02, Y, -0.22], color: '#2f4f8f', roughness: 0.2 },
      ],
      after: 450,
    },
    {
      run: [
        { op: 'add', name: 'Cup', primitive: { kind: 'vessel', profile: CUP, thickness: 0.014, segments: 64 }, translation: [-0.42, Y, 0.45], color: '#ead9c6', roughness: 0.6 },
        { op: 'duplicate', id: 'Cup', offset: [1.02, 0, 0.02] },
      ],
      after: 600,
    },
    { caption: 'Every batch is one undo step', hint: 'undo · redo' },
    { click: '[data-action=undo]', after: 450 },
    { click: '[data-action=redo]', after: 350 },
    { click: '#outliner li:nth-child(4)', after: 200 },
    { cursor: false },
    { caption: 'UI, chat and MCP agents share one command API', hint: 'GET /api/schema' },
    { frame: { ms: 1400, padding: 0.82, elevation: 20 } },
    { wait: 1800 },
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

export const SCENARIOS = { hero, modeling, agent, mcp, modifiers }
