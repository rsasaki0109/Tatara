import './style.css'
import * as THREE from 'three'
import { createApi } from './api.js'
import { Animator, Clock } from './clock.js'
import { Viewport, displayMesh, hasUvs } from './viewport.js'
import { NodeEditor, starterGraph } from './nodes.js'
import { UvEditor } from './uveditor.js'
import { PathPreview } from './pathpreview.js'
import { FinalRender } from './finalrender.js'
import { ProposalTray } from './proposals.js'

let chatBusy = false
import { Collaboration } from './collaboration.js'
import { HistoryPanel } from './history.js'
import { DemoRunner } from './demo.js'
import { SCENARIOS } from './scenarios.js'
import { icon } from './icons.js'
import { installWasmBackend } from './backend.js'
import { PROPERTIES, isAnimated, keyFrames, pose, track as trackOf } from './anim.js'
import { boneRotations, boneTrack } from './rig.js'

const params = new URLSearchParams(location.search)
const capture = params.has('capture')
// The browser-only build runs the Rust core as WebAssembly instead of a server.
const browserOnly = import.meta.env.VITE_TATARA_STATIC === '1' || params.has('wasm')
document.documentElement.classList.toggle('browser-only', browserOnly)
document.documentElement.classList.toggle('capture', capture)

const clock = new Clock(capture)
const animator = new Animator(clock)
const api = createApi(clock)
const $ = (id) => document.getElementById(id)

export const GLAZES = [
  ['Celadon', '#8fb9a0', 0.22],
  ['Tenmoku', '#3a2a22', 0.18],
  ['Shino', '#ead9c6', 0.6],
  ['Cobalt', '#2f4f8f', 0.2],
  ['Terracotta', '#b5643c', 0.78],
  ['Ash', '#d8d4cb', 0.7],
  ['Copper red', '#9b2c2c', 0.16],
  ['Bronze', '#b08d57', 0.32, 0.9],
]

// Assembly templates (src/assembly.rs Template).
const TEMPLATES = [
  ['table', 'Table'],
  ['chair', 'Chair'],
  ['lamp', 'Lamp'],
  ['mug', 'Mug'],
  ['plant', 'Plant'],
  ['shelf', 'Shelf'],
]

// Material presets (src/engine.rs MaterialPreset), with a swatch colour.
export const PRESETS = [
  ['glass', 'Glass', '#cfe3ee'],
  ['frosted', 'Frosted', '#e3e9ec'],
  ['chrome', 'Chrome', '#e9ecef'],
  ['steel', 'Steel', '#a9adb3'],
  ['gold', 'Gold', '#e8b04a'],
  ['copper', 'Copper', '#d9825b'],
  ['jade', 'Jade', '#4f9d7a'],
  ['ceramic', 'Ceramic', '#ece6da'],
  ['clay', 'Clay', '#b8714f'],
  ['plastic', 'Plastic', '#3f7fd8'],
  ['rubber', 'Rubber', '#26272b'],
  ['neon', 'Neon', '#ff4fd8'],
  ['wood', 'Wood', '#a0703f'],
  ['marble', 'Marble', '#efece6'],
  ['brick', 'Brick', '#a4452c'],
  ['tiles', 'Tiles', '#e8e4dc'],
]

// Procedural textures (src/texture.rs), each with a default second colour.
const PATTERNS = [
  ['none', 'None'],
  ['wood', 'Wood', '#6b4426'],
  ['marble', 'Marble', '#8f8a85'],
  ['brick', 'Brick', '#d8d0c4'],
  ['tiles', 'Tiles', '#8c867c'],
  ['checker', 'Checker', '#2b2d33'],
  ['stripes', 'Stripes', '#2b2d33'],
  ['image', 'Image…'],
  ['nodes', 'Nodes'],
]

const DEFAULT_VESSEL = [
  [0.16, 0],
  [0.27, 0.1],
  [0.33, 0.3],
  [0.25, 0.56],
  [0.13, 0.74],
  [0.15, 0.86],
]

const PRIMITIVES = {
  cube: { primitive: { kind: 'cube' }, y: 0.5, half: 0.5 },
  sphere: { primitive: { kind: 'sphere' }, y: 0.5, half: 0.5 },
  cylinder: { primitive: { kind: 'cylinder' }, y: 0.5, half: 0.5 },
  torus: { primitive: { kind: 'torus' }, y: 0.18, half: 0.68 },
  vessel: { primitive: { kind: 'vessel', profile: DEFAULT_VESSEL, thickness: 0.025, segments: 64 }, y: 0, half: 0.34 },
  plane: { primitive: { kind: 'plane', size: 2 }, y: 0.001, half: 1 },
}

const app = {
  clock,
  animator,
  api,
  capture,
  scene: { objects: [], revision: 0, next_id: 1 },
  history: { can_undo: false, can_redo: false },
  ai: false,
  selected: null,
  face: null,
  wireframe: false,
  inflight: 0,
  activity: [],
  extrudeDistance: 0.3,
  insetFraction: 0.3,
  bevelWidth: 0.1,
  // Animation playback.
  frame: 1,
  playing: false,
  playFrom: null,
  // Edit mode works on the selected object's base mesh.
  mode: 'object',
  selectMode: 'face',
  sel: { verts: [], edges: [], faces: [] },
  // Boolean card: the other object, and whether to keep it.
  boolWith: null,
  boolKeep: false,
  // Assemblies expanded in the outliner.
  openGroups: new Set(),
  // Sculpt mode brush (radius in world units).
  // `dynamic` turns on dynamic topology; `detail` is its edge length as a
  // fraction of the radius.
  brush: { brush: 'draw', radius: 0.15, strength: 0.5, invert: false, symmetry: 'x', dynamic: false, detail: 0.25 },
}

const viewport = new Viewport($('viewport'), clock, animator, {
  capture,
  onPick: (hit, e) => {
    if (hit && 'component' in hit) return pickComponent(hit.component, e.shiftKey)
    if (!hit) return select(null)
    if (e.altKey) select(hit.id, hit.face)
    else select(hit.id)
  },
  onTransform: (id, tf) => run(editCommands(id, tf), 'Gizmo').catch(() => {}),
  onSculpt: (id, stroke) => run([{ op: 'sculpt', id, ...stroke }], 'Sculpt').catch(() => viewport.revertSculpt(id)),
  // Animated bones get a key at this frame; static ones are posed.
  onReach: (id, bone, target) => {
    const o = objectById(id)
    const animated = (o?.tracks || []).some((t) => t.property === 'bone')
    run([{ op: 'reach', id, bone, target, ...(animated ? { frame: Math.round(app.frame) } : {}) }], 'Gizmo').catch(() => {})
  },
  onMoveVertices: (id, vertices, offset) =>
    run([{ op: 'move_vertices', id, vertices, offset: offset.map((v) => Math.round(v * 1e6) / 1e6) }], 'Gizmo').catch(() => {}),
})
app.viewport = viewport

// Rendered preview: the view path traced by the core, refining while still.
const preview = new PathPreview(viewport, clock, {
  budget: browserOnly ? 60000 : 360000,
  onUpdate: () => syncRenderLabel(),
})

app.preview = preview

// The Render panel: a finished image of this view.
const finalRender = new FinalRender($('final-render'), {
  camera: () => ({ camera: viewport.sceneCameraId, eye: viewport.camera.position.toArray(), target: viewport.controls.target.toArray(), up: viewport.camera.up.toArray(), fov: viewport.camera.fov }),
  focusPoint: () => {
    const node = viewport.nodes.get(app.selected)
    if (!node) return null
    const g = node.mesh.geometry
    if (!g.boundingSphere) g.computeBoundingSphere()
    return g.boundingSphere.center.clone().applyMatrix4(node.mesh.matrixWorld).toArray()
  },
  frame: () => Math.round(app.frame),
  range: () => animRange(),
  clock,
  onChange: () => {
    preview.paused = finalRender.running
    $('final-btn').classList.toggle('on', finalRender.open)
  },
})
app.finalRender = finalRender

// Proposals: changes offered for review. Hovering one shows the scene with
// it applied, its additions and changes outlined and what it removes as red
// ghosts.
const MARK = { added: 0x4fd18b, changed: 0xffb02e, removed: 0xff5a5a }
const proposalTray = new ProposalTray($('proposals'), {
  clock,
  // Thumbnails look from the viewer's angle, framed on the whole scene.
  camera: () => {
    const r = (v) => v.toArray().map((x) => x.toFixed(4)).join(',')
    const sphere = new THREE.Box3().setFromObject(viewport.root).getBoundingSphere(new THREE.Sphere())
    const dir = viewport.camera.position.clone().sub(viewport.controls.target).normalize()
    const fov = 30
    const distance = (Math.max(sphere.radius, 0.3) * 0.85) / Math.sin(THREE.MathUtils.degToRad(fov / 2))
    return { eye: r(sphere.center.clone().addScaledVector(dir, distance)), target: r(sphere.center), fov }
  },
  onPreview: (id) => showProposal(id),
  onDecide: (id, accept) => decideProposal(id, accept),
})
app.proposals = proposalTray
app.previewing = null
let previewToken = 0

async function showProposal(id) {
  const token = ++previewToken
  app.previewing = id
  if (id == null) {
    viewport.setHighlights(null)
    viewport.sync(app.scene, true)
    syncRenderLabel()
    return
  }
  const data = await api.proposal(id).catch(() => null)
  if (token !== previewToken || !data?.proposal) return
  const { proposal: p, scene } = data
  if (app.selected != null) select(null)
  const marks = new Map()
  for (const o of p.diff.added) marks.set(o.id, MARK.added)
  for (const o of p.diff.changed) marks.set(o.id, MARK.changed)
  for (const r of p.diff.removed) {
    const o = objectById(r.id)
    if (!o) continue
    scene.objects.push({ ...o, material: { ...o.material, color: '#ff5a5a', opacity: 0.28, transmission: 0, metalness: 0, emissive: '#000000', emissive_strength: 0, texture: null } })
    marks.set(o.id, MARK.removed)
  }
  viewport.sync(scene, true)
  viewport.setHighlights(marks)
  const label = document.querySelector('.view-label')
  label.innerHTML = `<span class="dot proposal"></span>Proposal · ${escapeHtml(p.title)}`
}

// History: the recipe of steps; a revised step previews the replayed scene,
// with what it changes outlined.
const historyPanel = new HistoryPanel($('history'), {
  api,
  summarize: (commands) => summarize(commands),
  onPreview: (scene, step) => {
    if (!scene) {
      viewport.setHighlights(null)
      viewport.sync(app.scene, true)
      syncRenderLabel()
      return
    }
    if (app.selected != null) select(null)
    const before = new Map(app.scene.objects.map((o) => [o.id, JSON.stringify([o.transform, o.material, o.mesh.vertices.length, o.modifiers])]))
    const marks = new Map()
    for (const o of scene.objects) {
      const was = before.get(o.id)
      if (was == null) marks.set(o.id, MARK.added)
      else if (was !== JSON.stringify([o.transform, o.material, o.mesh.vertices.length, o.modifiers])) marks.set(o.id, MARK.changed)
    }
    viewport.sync(scene, false)
    viewport.setHighlights(marks)
    document.querySelector('.view-label').innerHTML = `<span class="dot proposal"></span>History · step ${step} replayed`
  },
  onApplied: async (step) => {
    viewport.setHighlights(null)
    syncRenderLabel()
    await refresh(true)
    log('History', `revised step ${step}`)
  },
  onError: (message) => toast(message, 'error'),
})
app.historyPanel = historyPanel

async function decideProposal(id, accept) {
  const p = proposalTray.pending.find((x) => x.id === id)
  await track(async () => {
    try {
      const r = accept ? await api.acceptProposal(id) : await api.rejectProposal(id)
      await refresh(true)
      log('Review', `${accept ? 'accepted' : 'rejected'} ${p?.title ?? `proposal ${id}`}`, r.revision ?? app.scene.revision)
    } catch (e) {
      toast(e.message, 'error')
      await refresh(true)
    }
  })
}

function syncRenderLabel() {
  const label = document.querySelector('.view-label')
  const shading = !preview.active ? 'Studio' : preview.samples ? `Rendered · ${preview.samples} samples` : 'Rendered · tracing…'
  const shot = objectById(viewport.sceneCameraId)
  label.innerHTML = `<span class="dot"></span>${shot?.camera ? `Camera · ${escapeHtml(shot.name)} · Esc to orbit` : 'Perspective'} · ${shading}`
  $('shading-btn').classList.toggle('on', preview.active)
}

// ---------------------------------------------------------------------------
// Node editor: edits the node graph of one object's material.
// ---------------------------------------------------------------------------

const nodeEditor = new NodeEditor($('node-editor'), {
  images: () => Object.keys(app.scene.images || {}),
  onClose: () => {
    app.nodeTarget = null
    nodeEditor.hide()
  },
  onChange: (graph) => {
    const o = objectById(app.nodeTarget)
    if (!o) return
    const t = o.material.texture
    run([{ op: 'material', id: o.id, texture: { ...t, pattern: 'nodes', graph } }]).catch(() => {})
  },
})
nodeEditor.toast = (m) => toast(m, 'error')

const uvEditor = new UvEditor($('uv-editor'), {
  onClose: () => closeUv(),
  onUnwrap: (method) => app.uvTarget != null && run([{ op: 'unwrap', id: app.uvTarget, method }]).catch(() => {}),
  onTransform: (faces, t) => app.uvTarget != null && run([{ op: 'transform_uvs', id: app.uvTarget, faces, ...t }]).catch(() => {}),
})



// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

function objectById(id) {
  return app.scene.objects.find((o) => o.id === id)
}

let collaboration = null
let remoteRevision = 0
let connectionEpoch = 0
let allowRevisionReset = false
async function catchUp() {
  if (remoteRevision > app.scene.revision && !app.inflight && !collaboration?.interacting) await refresh(true)
}
async function refresh(animate = true) {
  const epoch = connectionEpoch
  const st = await api.state()
  if (epoch !== connectionEpoch) return
  if (st.scene.revision < app.scene.revision && !allowRevisionReset) return
  allowRevisionReset = false
  app.scene = st.scene
  if (remoteRevision === Infinity) remoteRevision = st.scene.revision
  app.history = st.history
  app.ai = st.ai
  if (app.selected != null && !objectById(app.selected)) {
    app.selected = null
    app.face = null
  }
  const sel = objectById(app.selected)
  if (sel && app.face != null && app.face >= sel.mesh.faces.length) app.face = null
  if (!sel) app.mode = 'object'
  if (sel) {
    const nv = sel.mesh.vertices.length
    app.sel.verts = app.sel.verts.filter((v) => v < nv)
    app.sel.edges = app.sel.edges.filter(([a, b]) => a < nv && b < nv)
    app.sel.faces = app.sel.faces.filter((f) => f < sel.mesh.faces.length)
  }
  proposalTray.set(st.proposals, app.scene.revision)
  if (app.tab === 'history' && historyPanel.open == null) historyPanel.load()
  if (app.previewing != null) showProposal(app.previewing)
  else viewport.sync(app.scene, animate)
  preview.setRevision(app.scene.revision)
  if (app.checks && app.checks.revision !== app.scene.revision && viewport.warned.size) {
    // The scene changed since the last inspection: drop its outlines.
    viewport.setWarnings([])
    $('checks-out').querySelector('.checks-summary')?.classList.add('stale')
  }
  syncEdit()
  collaboration?.set({ version: collaboration.version, peers: collaboration.peers })
}

/** Push edit-mode state to the viewport and redraw panels. */
function syncEdit() {
  const active = app.mode === 'edit' && app.selected != null
  viewport.setSculptState({ active: app.mode === 'sculpt' && app.selected != null, ...app.brush })
  viewport.setEditState({ active, mode: app.selectMode, ...app.sel })
  viewport.setSelection(app.selected, app.face)
  const rigged = objectById(app.selected)
  viewport.setBone(rigged?.bones?.length ? currentBone(rigged).name : null)
  if (!rigged?.bones?.length) app.ik = false
  viewport.setIk(app.ik ? { id: rigged.id, bone: currentBone(rigged).name } : null)
  viewport.setLinks(constraintLinks(objectById(app.selected)))
  render()
}

function clearComponents() {
  app.sel = { verts: [], edges: [], faces: [] }
}

function setMode(mode) {
  if (mode !== 'object' && (objectById(app.selected)?.camera || objectById(app.selected)?.light)) return toast('Cameras and lights use object mode')
  if (mode !== 'object' && app.selected == null) return toast(`Select an object to ${mode} it`)
  app.mode = mode
  clearComponents()
  if (mode === 'object') app.face = null
  syncEdit()
}

function setSelectMode(m) {
  app.selectMode = m
  clearComponents()
  app.face = null
  syncEdit()
}

const sameEdge = (e, f) => (e[0] === f[0] && e[1] === f[1]) || (e[0] === f[1] && e[1] === f[0])

/** Click on a vertex/edge/face in edit mode; Shift toggles into the selection. */
function pickComponent(c, additive) {
  if (!additive) clearComponents()
  if (c) {
    const toggle = (list, item, eq = (a, b) => a === b) => {
      const i = list.findIndex((x) => eq(x, item))
      if (i >= 0 && additive) list.splice(i, 1)
      else if (i < 0) list.push(item)
    }
    if (c.vertex !== undefined) toggle(app.sel.verts, c.vertex)
    if (c.edge) toggle(app.sel.edges, c.edge, sameEdge)
    if (c.face !== undefined) toggle(app.sel.faces, c.face)
  }
  app.face = app.selectMode === 'face' ? (app.sel.faces.at(-1) ?? null) : null
  syncEdit()
}

function selectAll() {
  const o = objectById(app.selected)
  if (!o || app.mode !== 'edit') return
  const m = o.mesh
  const allSelected =
    (app.selectMode === 'vertex' && app.sel.verts.length === m.vertices.length) ||
    (app.selectMode === 'face' && app.sel.faces.length === m.faces.length) ||
    (app.selectMode === 'edge' && app.sel.edges.length > 0 && app.sel.edges.length === meshEdges(m).length)
  clearComponents()
  if (!allSelected) {
    if (app.selectMode === 'vertex') app.sel.verts = m.vertices.map((_, i) => i)
    if (app.selectMode === 'face') app.sel.faces = m.faces.map((_, i) => i)
    if (app.selectMode === 'edge') app.sel.edges = meshEdges(m)
  }
  app.face = app.selectMode === 'face' ? (app.sel.faces.at(-1) ?? null) : null
  syncEdit()
}

function meshEdges(m) {
  const seen = new Set()
  const out = []
  for (const f of m.faces) {
    for (let k = 0; k < f.length; k++) {
      const a = f[k]
      const b = f[(k + 1) % f.length]
      const id = a < b ? `${a},${b}` : `${b},${a}`
      if (!seen.has(id)) {
        seen.add(id)
        out.push([a, b])
      }
    }
  }
  return out
}

/** Faces an extrude or inset acts on: the edit selection, or the Alt+clicked face. */
function targetFaces() {
  if (app.mode === 'edit' && app.selectMode === 'face') return app.sel.faces
  return app.face != null ? [app.face] : []
}

function summarize(commands) {
  const parts = []
  for (const c of commands) {
    const label =
      c.op === 'add'
        ? `add ${c.primitive.kind}`
        : c.op === 'build'
          ? `build ${c.template}`
          : c.op === 'boolean'
            ? c.operation
            : c.op === 'material' && c.preset
              ? `${c.preset} material`
              : c.op === 'material' && c.texture
                ? `${c.texture.pattern} texture`
                : c.op === 'add_image'
                ? `image ${c.name}`
                : c.op === 'world'
                ? `world${c.image ? ` ${c.image}` : c.sky ? ` ${c.sky}` : ''}`
                : c.op === 'add_modifier'
                ? `+ ${c.modifier.type}`
                : c.op === 'set_modifier'
                  ? `${c.modifier.type}`
                  : c.op
    const last = parts.at(-1)
    if (last && last.label === label) last.n++
    else parts.push({ label, n: 1 })
  }
  return parts.map((p) => (p.n > 1 ? `${p.label} ×${p.n}` : p.label)).join(' · ')
}

function log(source, text, revision = app.scene.revision) {
  app.activity.unshift({ source, text, revision })
  app.activity.length = Math.min(app.activity.length, 40)
  renderActivity()
}

async function track(fn) {
  app.inflight++
  try {
    return await fn()
  } finally {
    app.inflight--
    catchUp().catch(() => {})
  }
}

/** Apply a batch through the shared command API. */
async function run(commands, source = 'UI') {
  return track(async () => {
    try {
      const r = await api.commands(commands, collaboration?.revision ?? app.scene.revision, source)
      if (collaboration?.interacting) collaboration.revision = r.revision
      await refresh(true)
      log(source, summarize(commands), r.revision)
      if (r.rebased_from != null) toast('Applied your change alongside a newer edit.')
      return r
    } catch (e) {
      toast(e.message, 'error')
      if (e.status === 409) await refresh(true)
      throw e
    }
  })
}

async function history(kind) {
  return track(async () => {
    try {
      await api[kind]()
      await refresh(true)
      log('UI', kind)
    } catch (e) {
      toast(e.message, 'error')
    }
  })
}

function select(id, face = null) {
  if (id !== app.selected) clearComponents()
  app.selected = id
  if (objectById(id)?.camera || objectById(id)?.light) face = null
  app.face = id == null ? null : face
  if (id == null || objectById(id)?.camera || objectById(id)?.light) app.mode = 'object'
  if (face != null) {
    app.selectMode = 'face'
    app.sel = { verts: [], edges: [], faces: [face] }
  }
  syncEdit()
  collaboration?.send(true)
}

function freeSpot(half) {
  const box = viewport.sceneBounds()
  if (box.isEmpty()) return 0
  return Math.round((box.max.x + 0.35 + half) * 100) / 100
}

const actions = {
  async addLight() {
    const r = await run([{op:'add_light',translation:[1,1.7,1],lamp:{kind:'point',color:'#ffffff',intensity:20}}])
    select(r.created[0]); return r
  },
  async addCamera() {
    const c = viewport.camera
    const r = await run([{ op: 'add_camera', translation: c.position.toArray(), rotation: [c.rotation.x, c.rotation.y, c.rotation.z], lens: { fov: c.fov, aperture: 0, focus: c.position.distanceTo(viewport.controls.target) } }])
    select(r.created[0])
    return r
  },
  async add(kind) {
    const p = PRIMITIVES[kind]
    const r = await run([{ op: 'add', primitive: p.primitive, translation: [freeSpot(p.half), p.y, 0] }])
    select(r.created[0])
    return r
  },
  duplicate() {
    if (app.selected == null) return toast('Select an object first')
    return run([{ op: 'duplicate', id: app.selected, offset: [0.9, 0, 0] }]).then((r) => select(r.created[0]))
  },
  delete() {
    if (app.selected == null) return toast('Select an object first')
    return run([{ op: 'delete', id: app.selected }])
  },
  extrude(distance = app.extrudeDistance) {
    const faces = targetFaces()
    if (app.selected == null || !faces.length) return toast('Alt+click a face to extrude it')
    return run(faces.map((face) => ({ op: 'extrude', id: app.selected, face, distance })))
  },
  inset(fraction = app.insetFraction) {
    const faces = targetFaces()
    if (app.selected == null || !faces.length) return toast('Alt+click a face to inset it')
    return run(faces.map((face) => ({ op: 'inset', id: app.selected, face, fraction })))
  },
  bevel(width = app.bevelWidth) {
    if (app.selected == null) return toast('Select an object first')
    const picked = app.mode === 'edit' && app.selectMode === 'edge' && app.sel.edges.length > 0
    const cmd = { op: 'bevel', id: app.selected, width }
    if (picked) cmd.edges = app.sel.edges
    return run([cmd]).then((r) => {
      clearComponents()
      syncEdit()
      return r
    })
  },
  loopCut(fraction = 0.5) {
    const edge = app.sel.edges.at(-1)
    if (app.mode !== 'edit' || app.selectMode !== 'edge' || !edge) return toast('In edge mode, click an edge to cut across')
    return run([{ op: 'loop_cut', id: app.selected, edge, fraction }]).then((r) => {
      clearComponents()
      syncEdit()
      return r
    })
  },
  editMode: () => setMode(app.mode === 'edit' ? 'object' : 'edit'),
  selectMode: (m) => setSelectMode(m),
  selectAll,
  addModifier(type) {
    const o = objectById(app.selected)
    if (!o) return toast('Select an object first')
    return run([{ op: 'add_modifier', id: o.id, modifier: defaultModifier(type, o) }])
  },
  setModifier(index, modifier) {
    return run([{ op: 'set_modifier', id: app.selected, index, modifier }])
  },
  removeModifier(index) {
    return run([{ op: 'remove_modifier', id: app.selected, index }])
  },
  applyModifiers() {
    return run([{ op: 'apply_modifiers', id: app.selected }])
  },
  subdivide() {
    if (app.selected == null) return toast('Select an object first')
    return run([{ op: 'subdivide', id: app.selected, levels: 1 }])
  },
  undo: () => history('undo'),
  redo: () => history('redo'),
  async save() {
    const scene = await api.scene()
    const blob = new Blob([JSON.stringify(scene, null, 1)], { type: 'application/json' })
    download(URL.createObjectURL(blob), 'scene.tatara.json')
    toast('Saved scene.tatara.json')
  },
  open() {
    $('open-input').click()
  },
  exportObj() {
    download('/api/export/obj', 'scene.obj')
  },
  exportGlb() {
    download('/api/export/glb', 'scene.glb')
  },
  renderImage(on = !finalRender.open) {
    if (on) finalRender.show()
    else finalRender.hide()
    finalRender.onChange()
  },
  rendered(on = !preview.active) {
    preview.setActive(on)
  },
  wireframe(on = !app.wireframe) {
    app.wireframe = on
    viewport.setWireframe(on)
    render()
  },
  frame: () => { viewport.lookThrough(null); viewport.frameAll(); render() },
}
app.actions = actions
app.run = run
app.select = select
app.refresh = refresh
app.log = log
app.summarize = summarize
app.render = () => render()
app.setMode = setMode
app.setSelectMode = setSelectMode
app.pickComponent = pickComponent

async function download(href, name) {
  // Fetch first so /api downloads also work in the WebAssembly build.
  if (href.startsWith('/api/')) {
    try {
      const res = await fetch(href)
      if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
      href = URL.createObjectURL(await res.blob())
    } catch (e) {
      return toast(`Download failed: ${e.message}`, 'error')
    }
  }
  const a = document.createElement('a')
  a.href = href
  a.download = name
  document.body.append(a)
  a.click()
  a.remove()
}

$('open-input').addEventListener('change', async (e) => {
  const file = e.target.files[0]
  e.target.value = ''
  if (!file) return
  try {
    if (/\.(glb|gltf)$/i.test(file.name)) {
      // glTF is merged into the current scene as one undoable step.
      await track(async () => {
        const r = await api.importModel(await file.arrayBuffer())
        await refresh(true)
        if (r.created.length) select(r.created.at(-1))
        viewport.frameAll()
        log('UI', `import ${file.name} · ${r.created.length} object${r.created.length === 1 ? '' : 's'}`, r.revision)
      })
      return
    }
    const scene = JSON.parse(await file.text())
    await track(async () => {
      await api.putScene(scene)
      select(null)
      await refresh(false)
      viewport.frameAll()
      log('UI', `open ${file.name}`)
    })
  } catch (err) {
    toast(`Could not open ${file.name}: ${err.message}`, 'error')
  }
})

// An image file becomes a scene image and the selected object's texture.
$('image-input').addEventListener('change', async (e) => {
  const file = e.target.files[0]
  e.target.value = ''
  const o = objectById(app.imageTarget ?? app.selected)
  if (!file || !o) return
  const name = file.name.replace(/\.[^.]+$/, '').replace(/[^\w .-]+/g, ' ').trim().slice(0, 60) || 'Image'
  const data = await track(() =>
    clock.track(
      new Promise((resolve, reject) => {
        const reader = new FileReader()
        reader.onload = () => resolve(reader.result)
        reader.onerror = () => reject(reader.error)
        reader.readAsDataURL(file)
      }),
    ),
  )
  const old = o.material.texture
  // A picture usually covers a face once, like a label or a poster.
  const texture = { pattern: 'image', image: name, scale: old?.scale ?? 1, relief: old?.relief ?? 0, fit: true }
  run([{ op: 'add_image', name, data }, { op: 'material', id: o.id, texture }]).catch(() => {})
})

// A panorama becomes a scene image that lights the world.
$('world-input').addEventListener('change', async (e) => {
  const file = e.target.files[0]
  e.target.value = ''
  if (!file) return
  const name = file.name.replace(/\.[^.]+$/, '').replace(/[^\w .-]+/g, ' ').trim().slice(0, 60) || 'Environment'
  const data = await track(() =>
    clock.track(
      new Promise((resolve, reject) => {
        const reader = new FileReader()
        reader.onload = () => resolve(reader.result)
        reader.onerror = () => reject(reader.error)
        reader.readAsDataURL(file)
      }),
    ),
  )
  run([{ op: 'add_image', name, data }, { op: 'world', image: name }]).catch(() => {})
})

// ---------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------

function button(action, label, title, cls = '') {
  return `<button data-action="${action}" class="${cls}" title="${title}">${icon(action)}<span>${label}</span></button>`
}

$('add-group').innerHTML =
  `<span class="group-label">Add</span>` +
  Object.keys(PRIMITIVES)
    .map((k) => `<button data-add="${k}" title="Add ${k}">${icon(k)}<span>${k[0].toUpperCase() + k.slice(1)}</span></button>`)
    .join('') + button('addCamera', 'Camera', 'Add a scene camera from the current view') + button('addLight','Light','Add an editable point light')
$('edit-group').innerHTML =
  button('extrude', 'Extrude', 'Extrude selected face (E)') +
  button('inset', 'Inset', 'Inset selected face (I)') +
  button('subdivide', 'Subdivide', 'Catmull-Clark subdivision') +
  button('duplicate', 'Duplicate', 'Duplicate (Shift+D)') +
  button('delete', 'Delete', 'Delete (X)')
$('history-group').innerHTML =
  button('undo', '', 'Undo (Ctrl+Z)', 'icon-only') + button('redo', '', 'Redo (Ctrl+Shift+Z)', 'icon-only')
$('file-group').innerHTML =
  button('open', 'Open', 'Open a .tatara.json scene, or import .glb / .gltf') + button('save', 'Save', 'Save (Ctrl+S)') + button('exportGlb', 'GLB', 'Export glTF (.glb)') + button('exportObj', 'OBJ', 'Export OBJ')
$('wire-btn').innerHTML = icon('wire')
$('frame-btn').innerHTML = icon('frame')
$('shading-btn').innerHTML = icon('render')
$('final-btn').innerHTML = icon('camera')

document.addEventListener('click', (e) => {
  const look = e.target.closest('[data-look-camera]')
  if (look) { viewport.lookThrough(Number(look.dataset.lookCamera)); renderedKey = ''; return render() }
  if (e.target.closest('[data-exit-camera]')) { viewport.lookThrough(null); renderedKey = ''; return render() }
  const add = e.target.closest('[data-add]')
  if (add) return actions.add(add.dataset.add).catch(() => {})
  const act = e.target.closest('[data-action]')
  if (act) {
    const r = actions[act.dataset.action]()
    if (r?.catch) r.catch(() => {})
  }
})
$('wire-btn').addEventListener('click', () => actions.wireframe())
$('mode-bar').addEventListener('click', (e) => {
  const m = e.target.closest('[data-mode]')
  if (m && !m.disabled) return setMode(m.dataset.mode)
  const sm = e.target.closest('[data-select-mode]')
  if (sm) setSelectMode(sm.dataset.selectMode)
})
$('frame-btn').addEventListener('click', () => actions.frame())
$('shading-btn').addEventListener('click', () => actions.rendered())
$('final-btn').addEventListener('click', () => actions.renderImage())

for (const tab of document.querySelectorAll('.tabs button')) tab.addEventListener('click', () => showTab(tab.dataset.tab))
function showTab(name) {
  for (const t of document.querySelectorAll('.tabs button')) t.classList.toggle('active', t.dataset.tab === name)
  for (const p of document.querySelectorAll('.tab-panel')) p.classList.toggle('active', p.dataset.panel === name)
  app.tab = name
  if (name === 'history') return historyPanel.load()
  historyPanel.close()
}
app.showTab = showTab

const KIND_ICON = { camera: 'camera', light: 'light', quadsphere: 'sphere', cube: 'cube', sphere: 'sphere', cylinder: 'cylinder', torus: 'torus', vessel: 'vessel', plane: 'plane' }

const SELECT_MODES = [
  ['vertex', 'Vertex (1)'],
  ['edge', 'Edge (2)'],
  ['face', 'Face (3)'],
]

const BRUSHES = [
  ['draw', 'Draw', 'Raise the surface (Ctrl: carve)'],
  ['inflate', 'Inflate', 'Swell along the normals (Ctrl: shrink)'],
  ['smooth', 'Smooth', 'Relax bumps (or hold Shift)'],
  ['flatten', 'Flatten', 'Press onto a plane'],
  ['grab', 'Grab', 'Drag a region'],
]

function renderModeBar() {
  const edit = app.mode === 'edit'
  const mode = (m, label, title) =>
    `<button data-mode="${m}" class="${app.mode === m ? 'on' : ''}" ${m !== 'object' && (app.selected == null || objectById(app.selected)?.camera || objectById(app.selected)?.light) ? 'disabled' : ''} title="${title}">${icon(m)}${label}</button>`
  $('mode-bar').innerHTML =
    `<div class="mode-group">${mode('object', 'Object', 'Object mode')}${mode('edit', 'Edit', 'Edit mode (Tab)')}${mode('sculpt', 'Sculpt', 'Sculpt mode')}</div>` +
    (edit
      ? `<div class="mode-group">${SELECT_MODES.map(([m, t]) => `<button data-select-mode="${m}" class="icon-btn ${app.selectMode === m ? 'on' : ''}" title="${t}">${icon(`sel-${m}`)}</button>`).join('')}</div>`
      : '')
  $('status-hint').textContent = edit
    ? '1/2/3 vertex/edge/face · Shift+click add · A all · G move · Ctrl+B bevel · Ctrl+R loop cut · Tab exit'
    : app.mode === 'sculpt'
      ? 'Drag on the object to sculpt · Shift smooth · Ctrl invert · [ ] radius · Esc exit'
      : 'Alt+click a face · Tab edit mode · G/R/S transform · F frame · W wireframe'
}

function render() {
  const label = document.querySelector('.view-label')
  if (!label.querySelector('.proposal') && (viewport.sceneCameraId != null || label.textContent.startsWith('Camera'))) syncRenderLabel()
  renderModeBar()
  renderTimeline()
  const { objects } = app.scene
  const faces = objects.reduce((n, o) => n + displayMesh(o).faces.length, 0)
  $('object-count').textContent = objects.length ? String(objects.length) : ''
  $('outliner-empty').style.display = objects.length ? 'none' : ''
  const row = (o, cls = '') =>
    `<li data-id="${o.id}" class="${cls} ${o.id === app.selected ? 'selected' : ''}">${icon(KIND_ICON[o.kind] || 'cube')}<span class="name">${escapeHtml(o.group ? o.name.slice(o.group.length + 1) || o.name : o.name)}</span>${isAnimated(o) ? '<span class="keyed" title="Animated">◆</span>' : ''}<span class="swatch" style="background:${pose(o, app.frame).material.color}"></span></li>`
  // Assemblies collapse into one row; it opens while one of its parts is selected.
  const rows = []
  const listed = new Set()
  for (const o of objects) {
    if (!o.group) {
      rows.push(row(o))
      continue
    }
    if (listed.has(o.group)) continue
    listed.add(o.group)
    const parts = objects.filter((p) => p.group === o.group)
    const active = parts.some((p) => p.id === app.selected)
    const open = active || app.openGroups.has(o.group)
    rows.push(
      `<li data-group="${escapeHtml(o.group)}" class="group-row ${active ? 'active' : ''}"><span class="twisty" data-twisty>${open ? '▾' : '▸'}</span>${icon('group')}<span class="name">${escapeHtml(o.group)}</span><span class="muted count">${parts.length}</span><span class="swatch" style="background:${pose(parts[0], app.frame).material.color}"></span></li>`,
    )
    if (open) for (const p of parts) rows.push(row(p, 'part'))
  }
  $('outliner').innerHTML = rows.join('')
  $('status-rev').textContent = `Revision ${app.scene.revision}`
  $('status-mesh').textContent = `${objects.length} object${objects.length === 1 ? '' : 's'} · ${faces.toLocaleString('en-US')} faces`
  for (const b of document.querySelectorAll('[data-action=undo]')) b.disabled = !app.history.can_undo
  for (const b of document.querySelectorAll('[data-action=redo]')) b.disabled = !app.history.can_redo
  const sel = objectById(app.selected)
  for (const a of ['duplicate', 'delete', 'subdivide']) document.querySelector(`[data-action=${a}]`).disabled = !sel || (a === 'subdivide' && Boolean(sel.camera || sel.light))
  const noFaces = !sel || targetFaces().length === 0
  document.querySelector('[data-action=extrude]').disabled = noFaces
  document.querySelector('[data-action=inset]').disabled = noFaces
  $('wire-btn').classList.toggle('on', app.wireframe)
  $('ai-badge').textContent = app.replaying ? 'replay' : app.ai ? 'on' : 'off'
  $('ai-badge').classList.toggle('on', app.ai || Boolean(app.replaying))
  $('chat-input').disabled = !app.ai && !app.replaying
  $('chat-send').disabled = !app.ai || chatBusy
  $('chat-review').disabled = chatBusy
  $('chat-status').textContent = app.ai || app.replaying ? $('chat-status').textContent : 'Set TATARA_AI_* on the server to enable'
  renderProperties(sel)
  syncNodeEditor()
  syncUvEditor()
}

function openUv(o) {
  app.uvTarget = o.id
  uvEditor.show(o)
  viewport.setUvPreview(o.id)
}

function closeUv() {
  app.uvTarget = null
  uvEditor.hide()
  viewport.setUvPreview(null)
}

/** Keep the UV editor on its object, or close it if the object is gone. */
function syncUvEditor() {
  if (!uvEditor.open) return
  const o = objectById(app.uvTarget)
  if (!o) return closeUv()
  uvEditor.set(o)
}

function openNodes(o) {
  app.nodeTarget = o.id
  nodeEditor.show(o.material.texture.graph, o.name)
}

/** Keep the node editor on its object's current graph, or close it. */
function syncNodeEditor() {
  if (!nodeEditor.open) return
  const o = objectById(app.nodeTarget)
  const graph = o?.material.texture?.pattern === 'nodes' ? o.material.texture.graph : null
  if (!graph) {
    app.nodeTarget = null
    return nodeEditor.hide()
  }
  if (JSON.stringify(graph) !== JSON.stringify(nodeEditor.graph)) nodeEditor.set(graph)
}

$('outliner').addEventListener('click', (e) => {
  const group = e.target.closest('li[data-group]')
  if (group) {
    const name = group.dataset.group
    if (e.target.closest('[data-twisty]')) {
      if (app.openGroups.has(name)) app.openGroups.delete(name)
      else app.openGroups.add(name)
      return render()
    }
    const first = app.scene.objects.find((o) => o.group === name)
    if (first) select(first.id)
    return
  }
  const li = e.target.closest('li[data-id]')
  if (li) select(Number(li.dataset.id))
})

// The world: built-in skies, environment images and their settings.
const SKIES = [
  ['studio', 'Studio', '#5a5c64'],
  ['daylight', 'Daylight', '#78aee8'],
  ['sunset', 'Sunset', '#f0894a'],
  ['overcast', 'Overcast', '#b9bec7'],
  ['night', 'Night', '#2a3558'],
]
const worldOf = () => ({ sky: 'studio', image: null, strength: 1, rotation: 0, background: false, ...app.scene.world })

function worldCard() {
  const w = worldOf()
  const images = Object.entries(app.scene.images || {})
  // Panoramas: HDR files, and any 2:1 picture.
  const panoramas = images.filter(([, i]) => i.mime === 'image/vnd.radiance' || i.width === i.height * 2)
  const skies = SKIES.map(([id, label, c]) => `<button class="chip preset ${!w.image && w.sky === id ? 'on' : ''}" data-sky="${id}" title="${label} sky"><i style="--c:${c}"></i>${label}</button>`).join('')
  const maps = panoramas
    .map(([name]) => `<button class="chip preset ${w.image === name ? 'on' : ''}" data-world-image="${escapeHtml(name)}" title="Light with ${escapeHtml(name)}"><i style="--c:#c9a35a"></i>${escapeHtml(name)}</button>`)
    .join('')
  return `<div class="card world-card"><div class="card-title">World <span class="muted small">${escapeHtml(w.image ?? w.sky)} · lighting</span></div>
    <div class="presets">${skies}${maps}<button class="chip" data-world-hdri title="Light the scene with an HDRI panorama (.hdr, or a 2:1 PNG/JPEG)">+ HDRI…</button></div>
    <label class="slider"><span>Strength</span><input type="range" min="0" max="4" step="0.05" id="w-strength" value="${w.strength}"><b>${fmt(w.strength)}</b></label>
    <label class="slider"><span>Rotation</span><input type="range" min="0" max="360" step="1" id="w-rotation" value="${Math.round(w.rotation)}"><b>${Math.round(w.rotation)}°</b></label>
    <label class="inline world-bg"><input type="checkbox" id="w-bg" ${w.background ? 'checked' : ''}> Show as background</label>
  </div>`
}

function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c])
}

const fmt = (v, d = 2) => (Math.round(v * 10 ** d) / 10 ** d).toString()
let renderedKey = ''

function renderProperties(o) {
  const el = $('properties')
  const key = o
    ? JSON.stringify([o.id, o.name, o.camera, o.light, viewport.sceneCameraId, o.transform, o.material, o.tracks, app.playing ? 'playing' : app.frame, o.mesh.faces.length, displayMesh(o).faces.length, Boolean(o.mesh.uvs?.length), o.mesh.seams?.length, o.bones, app.bone, app.ik, app.scene.constraints, app.scene.arrangements, app.constraintKind, o.group, o.modifiers, o.smooth, app.face, app.mode, app.selectMode, app.sel, app.brush, app.boolWith, app.boolKeep, app.scene.objects.map((x) => x.name)])
    : `none:${JSON.stringify([app.scene.world, Object.keys(app.scene.images || {})])}`
  if (key === renderedKey) return
  if (o && el.contains(document.activeElement) && document.activeElement.tagName === 'INPUT' && renderedKey.startsWith(`[${o.id},`)) return
  renderedKey = key
  if (!o) {
    el.innerHTML = `<div class="empty-props">${icon('cursor')}<p>Select an object in the viewport or outliner.</p><p class="muted">Alt+click selects a single face for extrusion.</p></div>
      <div class="card"><div class="card-title">Build an assembly <span class="muted small">build · place · arrange</span></div>
      <div class="brushes">${TEMPLATES.map(([t, label]) => `<button class="chip" data-build="${t}">+ ${label}</button>`).join('')}</div></div>
      ${worldCard()}`
    return
  }
  // Animated properties show their value at the current frame, marked ◆.
  const shown = pose(o, app.frame)
  const t = shown.transform
  const keyed = (p) => (trackOf(o, p) ? ' <span class="keyed" title="Animated: edits key this frame">◆</span>' : '')
  const vec = (field, values, step, conv = (x) => x) =>
    values
      .map((v, i) => `<label class="axis axis-${'xyz'[i]}"><i>${'XYZ'[i]}</i><input type="number" step="${step}" data-field="${field}" data-i="${i}" value="${fmt(conv(v), 3)}"></label>`)
      .join('')
  if (o.light) {
    el.innerHTML = `<div class="card"><div class="card-title">Light <span class="muted small">#${o.id}</span></div>
      <input class="name-input" id="p-name" value="${escapeHtml(o.name)}" spellcheck="false"></div>
      <div class="card"><div class="card-title">Transform</div>
      <div class="vec-row"><span>Location${keyed('translation')}</span>${vec('translation',t.translation,.1)}</div>
      <div class="vec-row"><span>Rotation°${keyed('rotation')}</span>${vec('rotation',t.rotation,5,r=>r*180/Math.PI)}</div></div>
      <div class="card"><div class="card-title">Lighting</div>
      <label class="row">Type<select id="light-kind"><option value="point" ${o.light.kind==='point'?'selected':''}>Point</option><option value="sun" ${o.light.kind==='sun'?'selected':''}>Sun</option></select></label>
      <label class="row">Colour<input type="color" id="light-color" value="${o.light.color}"></label>
      <label class="vec-row"><span>Intensity (${o.light.kind==='point'?'cd':'lx'})</span><input type="number" id="light-intensity" min="0" max="100000" step=".1" value="${o.light.intensity}"></label>
      <p class="muted small">Point lights fall off with distance squared. Sun rays travel down local −Z. Intensity 0 switches the light off. Transform tracks animate the light.</p></div>`
    return
  }
  if (o.camera) {
    el.innerHTML = `<div class="card"><div class="card-title">Camera <span class="muted small">#${o.id}</span></div>
      <input class="name-input" id="p-name" value="${escapeHtml(o.name)}" spellcheck="false">
      <div class="row"><button class="small-btn" data-look-camera="${o.id}">Look through</button><button class="small-btn" data-exit-camera>Return to orbit</button></div></div>
      <div class="card"><div class="card-title">Transform</div>
      <div class="vec-row"><span>Location${keyed('translation')}</span>${vec('translation', t.translation, .1)}</div>
      <div class="vec-row"><span>Rotation°${keyed('rotation')}</span>${vec('rotation', t.rotation, 5, r => r * 180 / Math.PI)}</div></div>
      <div class="card"><div class="card-title">Perspective lens</div>
      ${[['fov','Vertical FOV°',1,170,.1],['aperture','Lens radius m',0,1,.01],['focus','Focus m',.001,10000,.1]].map(([f,label,min,max,step]) => `<label class="vec-row"><span>${label}</span><input type="number" id="camera-${f}" data-camera-lens="${f}" min="${min}" max="${max}" step="${step}" value="${o.camera[f]}"></label>`).join('')}
      <p class="muted small">Looks along local −Z. Lens and animation are used by Render while looking through this camera.</p></div>`
    return
  }
  const glazes = GLAZES.map(
    ([name, color, rough, metal = 0]) =>
      `<button class="glaze ${shown.material.color === color ? 'on' : ''}" data-glaze="${color},${rough},${metal}" title="${name}" style="--c:${color}"></button>`,
  ).join('')
  const m = shown.material
  const presets = PRESETS.map(
    ([id, name, swatch]) => `<button class="chip preset" data-preset="${id}" title="${name} preset"><i style="--c:${swatch}"></i>${name}</button>`,
  ).join('')
  const b = app.brush
  const others = app.scene.objects.filter((x) => x.id !== o.id && !x.camera && !x.light)
  if (!others.some((x) => x.id === app.boolWith)) app.boolWith = others.at(-1)?.id ?? null
  const brushCard =
    app.mode === 'sculpt'
      ? `<div class="card brush-card">
      <div class="card-title">Brush <span class="muted small">${num(o.mesh.faces.length)} faces</span></div>
      <div class="brushes">${BRUSHES.map(([id, label, title]) => `<button class="chip ${b.brush === id ? 'on' : ''}" data-brush="${id}" title="${title}">${label}</button>`).join('')}</div>
      <label class="slider"><span>Radius</span><input type="range" min="0.02" max="1" step="0.01" id="b-radius" value="${b.radius}"><b>${fmt(b.radius)}</b></label>
      <label class="slider"><span>Strength</span><input type="range" min="0.05" max="1" step="0.01" id="b-strength" value="${b.strength}"><b>${fmt(b.strength)}</b></label>
      <div class="row">
        <button class="chip ${b.invert ? 'on' : ''}" data-brush-toggle="invert" title="Carve instead of raise (or hold Ctrl)">Invert</button>
        <button class="chip ${b.symmetry ? 'on' : ''}" data-brush-toggle="symmetry" title="Mirror strokes across X">Mirror X</button>
        <button class="small-btn push" data-action="subdivide" title="Subdivide the whole mesh">${icon('subdivide')}Add detail</button>
      </div>
      <div class="row">
        <button class="chip ${b.dynamic ? 'on' : ''}" data-brush-toggle="dynamic" title="Split edges under the brush so detail appears where you sculpt (dynamic topology)">Dynamic detail</button>
        <span class="muted small">${b.dynamic ? 'refines under the brush' : 'off: topology stays fixed'}</span>
      </div>
      ${
        b.dynamic
          ? `<label class="slider"><span>Detail</span><input type="range" min="0.1" max="0.5" step="0.01" id="b-detail" value="${b.detail}" title="Edge length as a fraction of the radius: smaller is finer"><b>${fmt(b.detail)}</b></label>`
          : o.mesh.faces.length < 1500
            ? '<div class="muted small">Low-poly meshes sculpt coarsely: add detail first, or turn on dynamic detail.</div>'
            : ''
      }
    </div>`
      : ''
  el.innerHTML = `${brushCard}
    <div class="card">
      <div class="card-title">Object <span class="muted small">#${o.id} · ${o.kind}</span></div>
      <input class="name-input" id="p-name" value="${escapeHtml(o.name)}" spellcheck="false">
      <div class="meta">${num(displayMesh(o).vertices.length)} vertices · ${num(displayMesh(o).faces.length)} faces${o.display ? ` <span class="muted">(base ${num(o.mesh.faces.length)})</span>` : ''}</div>
      ${o.group ? `<div class="row group-info">${icon('group')}<span>Part of <b>${escapeHtml(o.group)}</b></span><button class="chip push" data-group-act="turn" title="Turn the whole assembly 90°">↻ 90°</button><button class="chip" data-group-act="drop" title="Settle the whole assembly">Drop</button><button class="chip" data-group-act="delete" title="Delete every part">Delete</button></div>` : ''}
      <div class="row"><button class="chip ${o.smooth ? 'on' : ''}" data-shade title="Blend normals across every edge (Shade Smooth)">Smooth shading</button><button class="chip" data-drop title="Settle onto the floor or the object below">Drop to surface</button></div>
    </div>
    <div class="card">
      <div class="card-title">Transform</div>
      <div class="vec-row"><span>Location${keyed('translation')}</span>${vec('translation', t.translation, 0.1)}</div>
      <div class="vec-row"><span>Rotation°${keyed('rotation')}</span>${vec('rotation', t.rotation, 5, (r) => (r * 180) / Math.PI)}</div>
      <div class="vec-row"><span>Scale${keyed('scale')}</span>${vec('scale', t.scale, 0.1)}</div>
    </div>
    ${constraintCard(o)}
    ${arrangementCard(o)}
    <div class="card">
      <div class="card-title">Material${keyed('color')}</div>
      <div class="glazes">${glazes}</div>
      <div class="presets">${presets}</div>
      <div class="row"><input type="color" id="p-color" value="${shown.material.color}"><code class="muted">${shown.material.color}</code></div>
      <label class="slider"><span>Roughness</span><input type="range" min="0" max="1" step="0.01" id="p-rough" value="${shown.material.roughness}"><b>${fmt(shown.material.roughness)}</b></label>
      <label class="slider"><span>Metalness</span><input type="range" min="0" max="1" step="0.01" id="p-metal" value="${shown.material.metalness}"><b>${fmt(shown.material.metalness)}</b></label>
      <label class="slider"><span>Glass</span><input type="range" min="0" max="1" step="0.01" id="p-trans" value="${m.transmission}"><b>${fmt(m.transmission)}</b></label>
      <label class="slider"><span>Opacity${keyed('opacity')}</span><input type="range" min="0" max="1" step="0.01" id="p-opacity" value="${m.opacity}"><b>${fmt(m.opacity)}</b></label>
      <div class="row texture-row"><span>Texture</span>${PATTERNS.map(([id, label]) => `<button class="chip ${(m.texture?.pattern ?? 'none') === id ? 'on' : ''}" data-pattern="${id}">${label}</button>`).join('')}</div>
      ${m.texture ? textureControls(m.texture, hasUvs(displayMesh(o))) : ''}
      <div class="row emission"><span>Emission${keyed('emissive') || keyed('emissive_strength')}</span><input type="color" id="p-emissive" value="${m.emissive}"><input type="range" min="0" max="10" step="0.1" id="p-emit" value="${m.emissive_strength}" title="Strength"><b>${fmt(m.emissive_strength, 1)}</b></div>
    </div>
    <div class="card">
      <div class="card-title">Mesh</div>
      <div class="face-info ${selectionText() ? 'on' : ''}">${selectionText() || (app.mode === 'edit' ? `Click ${app.selectMode === 'edge' ? 'an edge' : `a ${app.selectMode}`} · Shift+click to add` : 'Alt+click a face, or press Tab to edit')}</div>
      <div class="row">
        <label class="inline">Distance <input type="text" inputmode="decimal" id="p-dist" value="${app.extrudeDistance}"></label>
        <button class="small-btn" data-action="extrude" ${targetFaces().length ? '' : 'disabled'}>${icon('extrude')}Extrude</button>
      </div>
      <div class="row">
        <label class="inline">Fraction <input type="text" inputmode="decimal" id="p-inset" value="${app.insetFraction}"></label>
        <button class="small-btn" data-action="inset" ${targetFaces().length ? '' : 'disabled'}>${icon('inset')}Inset</button>
      </div>
      <div class="row">
        <label class="inline">Width <input type="text" inputmode="decimal" id="p-bevel" value="${app.bevelWidth}"></label>
        <button class="small-btn" data-action="bevel" title="Bevel selected edges, or all edges (Ctrl+B)">${icon('bevel')}${app.mode === 'edit' && app.selectMode === 'edge' && app.sel.edges.length ? 'Bevel' : 'Bevel all'}</button>
      </div>
      <div class="row"><button class="small-btn wide" data-action="loopCut" ${app.mode === 'edit' && app.selectMode === 'edge' && app.sel.edges.length ? '' : 'disabled'} title="Cut a loop across the selected edge (Ctrl+R)">${icon('loopcut')}Loop cut</button></div>
    </div>
    <div class="card">
      <div class="card-title">UV <span class="muted small">${o.mesh.uvs?.length ? 'unwrapped' : 'projected'}${o.mesh.seams?.length ? ` · ${o.mesh.seams.length} seams` : ''}</span></div>
      <div class="row"><button class="small-btn" data-uv-open title="Open the UV editor">UV editor</button>${
        app.mode === 'edit' && app.selectMode === 'edge' && app.sel.edges.length
          ? '<button class="chip" data-seam="mark" title="Cut the UVs along the selected edges">Mark seam</button><button class="chip" data-seam="clear">Clear seam</button>'
          : '<span class="muted small">Edge-select in edit mode to mark seams</span>'
      }</div>
    </div>
    ${rigCard(o)}
    <div class="card boolean">
      <div class="card-title">Boolean <span class="muted small">cut · merge · overlap</span></div>
      ${
        others.length
          ? `<div class="row"><label class="inline">With <select id="bool-with">${others.map((x) => `<option value="${x.id}" ${x.id === app.boolWith ? 'selected' : ''}>${escapeHtml(x.name)}</option>`).join('')}</select></label>
      <label class="inline keep"><input type="checkbox" id="bool-keep" ${app.boolKeep ? 'checked' : ''}> Keep</label></div>
      <div class="row bool-ops"><button class="small-btn" data-bool="difference" title="Cut the other shape out of this one">${icon('bool-difference')}Difference</button><button class="small-btn" data-bool="union" title="Merge the other shape into this one">${icon('bool-union')}Union</button><button class="small-btn" data-bool="intersect" title="Keep only where they overlap">${icon('bool-intersect')}Intersect</button></div>`
          : '<div class="muted small">Add a second object to cut or merge with.</div>'
      }
    </div>
    <div class="card" id="modifiers">
      <div class="card-title">Modifiers <span class="muted small">non-destructive</span></div>
      ${o.modifiers.map(modifierCard).join('')}
      <div class="mod-add">${MODIFIER_TYPES.map((t) => `<button class="chip" data-add-mod="${t}">+ ${MODIFIER_LABEL[t]}</button>`).join('')}</div>
      ${o.modifiers.length ? `<button class="small-btn wide" data-mod-apply>${icon('subdivide')}Apply stack to base mesh</button>` : ''}
    </div>`
}

/** Second colour or image, tile size (box projection only) and relief. */
function textureControls(t, ownUvs) {
  const lead =
    t.pattern === 'nodes'
      ? '<button class="chip on" data-edit-nodes title="Open the node editor">Edit nodes</button>'
      : t.pattern === 'image'
      ? `<code class="image-name" title="Image">${escapeHtml(t.image)}</code>`
      : t.pattern === 'none'
        ? '<span></span>'
        : `<input type="color" id="p-color2" value="${t.color2}" title="Second colour">`
  const fit = `<button class="chip ${t.fit ? 'on' : ''}" data-tex-fit title="Stretch one tile over each side">Fit</button>`
  const size = ownUvs
    ? '<span class="muted small">Mesh UVs</span><b></b>'
    : t.fit
      ? `<span class="muted small">One per side</span>${fit}`
      : `<input type="range" min="0.05" max="3" step="0.05" id="p-tscale" value="${t.scale}" title="Metres per tile">${fit}`
  const normal = t.normal_map ? ` <span class="muted small">normal map ${escapeHtml(t.normal_map)}</span>` : ''
  return `<div class="row tex-size"><span>Tile size</span>${lead}${size}</div>
      <label class="slider"><span>Relief${normal}</span><input type="range" min="0" max="1" step="0.05" id="p-relief" value="${t.relief ?? 0}" ${t.normal_map ? 'disabled' : ''}><b>${fmt(t.relief ?? 0)}</b></label>`
}

const num = (n) => n.toLocaleString('en-US')

function selectionText() {
  const plural = (n, w) => `<b>${n}</b> ${w}${n === 1 ? '' : 's'} selected`
  if (app.mode === 'edit') {
    if (app.selectMode === 'vertex' && app.sel.verts.length) return plural(app.sel.verts.length, 'vertex').replace('vertexs', 'vertices')
    if (app.selectMode === 'edge' && app.sel.edges.length) return plural(app.sel.edges.length, 'edge')
    if (app.selectMode === 'face' && app.sel.faces.length) return plural(app.sel.faces.length, 'face')
    return ''
  }
  return app.face != null ? `Face <b>${app.face}</b> selected` : ''
}

const MODIFIER_TYPES = ['mirror', 'subdivision', 'array', 'twist', 'taper']
const MODIFIER_LABEL = { mirror: 'Mirror', subdivision: 'Subdivision', array: 'Array', twist: 'Twist', taper: 'Taper' }

function defaultModifier(type, o) {
  const xs = o.mesh.vertices.map((v) => v[1])
  const height = Math.max(...xs) - Math.min(...xs) || 1
  switch (type) {
    case 'mirror':
      return { type, axis: 'x' }
    case 'subdivision':
      return { type, levels: 1 }
    case 'array':
      return { type, count: 3, offset: [0, Math.round(height * 1.15 * 100) / 100, 0] }
    case 'twist':
      return { type, angle: Math.PI / 2 }
    default:
      return { type: 'taper', factor: 0.5 }
  }
}

function modifierCard(m, i) {
  const field = (name, value, extra = '') =>
    `<input type="text" inputmode="decimal" class="mod-num" data-mod="${i}" data-field="${name}" value="${value}" ${extra}>`
  const segment = (name, options, current) =>
    `<div class="segment">${options
      .map((v) => `<button class="${String(v) === String(current) ? 'on' : ''}" data-mod="${i}" data-set="${name}" data-value="${v}">${String(v).toUpperCase()}</button>`)
      .join('')}</div>`
  let body = ''
  if (m.type === 'mirror') body = `<div class="mod-row"><span>Axis</span>${segment('axis', ['x', 'y', 'z'], m.axis)}</div>`
  if (m.type === 'subdivision') body = `<div class="mod-row"><span>Levels</span>${segment('levels', [1, 2, 3, 4], m.levels)}</div>`
  if (m.type === 'array') {
    body =
      `<div class="mod-row"><span>Count</span>${field('count', m.count)}</div>` +
      `<div class="vec-row"><span>Offset</span>${m.offset
        .map((v, k) => `<label class="axis axis-${'xyz'[k]}"><i>${'XYZ'[k]}</i>${field(`offset.${k}`, fmt(v, 3))}</label>`)
        .join('')}</div>`
  }
  if (m.type === 'twist') {
    const deg = Math.round((m.angle * 180) / Math.PI)
    body = `<label class="slider"><span>Angle°</span><input type="range" min="-360" max="360" step="5" data-mod="${i}" data-field="angle" value="${deg}"><b>${deg}</b></label>`
  }
  if (m.type === 'taper') {
    body = `<label class="slider"><span>Factor</span><input type="range" min="0" max="2" step="0.05" data-mod="${i}" data-field="factor" value="${m.factor}"><b>${fmt(m.factor)}</b></label>`
  }
  return `<div class="mod"><div class="mod-head">${icon(`mod-${m.type}`)}<b>${MODIFIER_LABEL[m.type]}</b><span class="muted small">${i + 1}</span><button class="mod-x" data-mod-remove="${i}" title="Remove">×</button></div>${body}</div>`
}

/** The bone the Rig card works on: the chosen one, or the first. */
function currentBone(o) {
  return o.bones?.find((b) => b.name === app.bone) || o.bones?.[0] || null
}

// Constraints: relations kept true through every edit (src/constraint.rs).
// A part of a built assembly is constrained as its whole group.
const subjectOf = (o) => o.group ?? o.id
const whoName = (who) => (typeof who === 'number' ? (objectById(who)?.name ?? `#${who}`) : who)
const involves = (o, who) => who === o.id || (o.group != null && who === o.group)
const RULES = { on: 'Keep on', mirrors: 'Mirror of', matches: 'Match look of', distance: 'Keep distance', align: 'Keep aligned' }

const LINK = { on: 0x4fd1c5, mirrors: 0xa78bfa, matches: 0xffb02e, distance: 0x67b8ff, align: 0xe4bd69 }
const idsOf = (who) => app.scene.objects.filter((x) => x.id === who || (x.group != null && x.group === who)).map((x) => x.id)

/** Dashed lines from the selection to what its constraints tie it to. */
function constraintLinks(o) {
  if (!o) return []
  return (app.scene.constraints || [])
    .filter((c) => involves(o, c.subject) || involves(o, c.support ?? c.of))
    .map((c) => ({ from: idsOf(c.subject), to: idsOf(c.support ?? c.of), color: LINK[c.kind] }))
    .concat((app.scene.arrangements || []).filter((a) => a.around != null && (involves(o, a.around) || a.items.some((w) => involves(o, w))))
      .flatMap((a) => a.items.map((w) => ({from: idsOf(w), to: idsOf(a.around), color: 0x87d39f}))))
}

/** The selected object leads a maintained row, grid or circle of other items. */
function arrangementCard(o) {
  const self = subjectOf(o)
  const targets = [...new Set(app.scene.objects.filter(x => !x.camera && !x.light).map(subjectOf))].filter((w) => w !== self)
  const chosen = app.layoutItems ?? targets
  const rows = (app.scene.arrangements || []).filter((a) => involves(o, a.around) || a.items.some((w) => involves(o, w)))
    .map((a) => `<div class="arrangement-row"><span>${escapeHtml(a.layout)} · ${a.items.length} items · ${a.around != null ? `around ${escapeHtml(whoName(a.around))}` : 'fixed centre'}</span><button class="chip" data-unarrange="${a.id}" title="Stop maintaining this layout">✕</button></div>`).join('')
  const controls = targets.length ? `<details class="arrangement-create"><summary>Arrange around ${escapeHtml(whoName(self))}</summary>
    <div class="arrangement-items">${targets.map((w) => `<label><input type="checkbox" data-layout-item="${escapeHtml(JSON.stringify(w))}" ${chosen.includes(w) ? 'checked' : ''}>${escapeHtml(whoName(w))}</label>`).join('')}</div>
    <label class="row small">Layout<select id="layout-kind">${['row', 'grid', 'circle'].map((k) => `<option value="${k}" ${k === (app.layoutKind ?? 'circle') ? 'selected' : ''}>${k}</option>`).join('')}</select></label>
    <label class="row small">Spacing (m)<input id="layout-spacing" type="number" min="0" max="100" step="any" required value="${app.layoutSpacing ?? 0.25}"></label>
    <label class="row small">Circle radius (m)<input id="layout-radius" type="number" min="0" max="100" step="any" placeholder="Auto" value="${app.layoutRadius ?? ''}"></label>
    <button class="small-btn" data-keep-layout>Keep layout</button></details>` : ''
  return rows || controls ? `<div class="card arrangements"><div class="card-title">Arrangements <span class="muted small">kept through every edit</span></div>${rows}${controls}</div>` : ''
}

function constraintCard(o) {
  const mine = (app.scene.constraints || []).filter((c) => involves(o, c.subject) || involves(o, c.support ?? c.of))
  const describe = (c) => {
    const self = involves(o, c.subject)
    const other = whoName(self ? (c.support ?? c.of) : c.subject)
    if (c.kind === 'on') return self ? `On ${other}` : `${other} rests on it`
    if (c.kind === 'mirrors') return `Mirrors ${other}${c.axis === 'z' ? ' (front/back)' : ''}`
    if (c.kind === 'distance') return `${c.distance} m from ${other}`
    if (c.kind === 'align') return `Aligned ${c.axes.toUpperCase()} with ${other}`
    return `Matches ${other}`
  }
  const rows = mine
    .map((c) => `<div class="constraint-row"><span class="c-kind c-${c.kind}">${c.kind === 'on' ? '⤓' : c.kind === 'mirrors' ? '⇋' : c.kind === 'distance' ? '↔' : '≡'}</span><span>${escapeHtml(describe(c))}</span><button class="chip" data-unconstrain="${escapeHtml(JSON.stringify([c.subject, c.kind]))}" title="Stop keeping this">✕</button></div>`)
    .join('')
  const self = subjectOf(o)
  const seen = new Set()
  const targets = []
  for (const x of app.scene.objects) {
    const who = x.group ?? x.id
    if (x.camera || x.light || who === self || seen.has(who)) continue
    seen.add(who)
    targets.push(who)
  }
  const kind = app.constraintKind ?? 'on'
  const add = targets.length
    ? `<div class="row constraint-add"><select id="c-kind">${Object.entries(RULES).map(([k, label]) => `<option value="${k}" ${k === kind ? 'selected' : ''}>${label}</option>`).join('')}</select><select id="c-target">${targets.map((w) => `<option value="${escapeHtml(JSON.stringify(w))}">${escapeHtml(whoName(w))}</option>`).join('')}</select><button class="small-btn" data-constrain>Keep</button></div>`
    : ''
  const axes = kind === 'align' && targets.length
    ? `<label class="row small">World axes<select id="c-align">${['x', 'y', 'z', 'xy', 'xz', 'yz', 'xyz'].map((a) => `<option value="${a}" ${a === (app.constraintAxes ?? 'y') ? 'selected' : ''}>${a.toUpperCase()}</option>`).join('')}</select></label>`
    : ''
  return `<div class="card constraints"><div class="card-title">Constraints <span class="muted small">${o.group ? `the whole ${escapeHtml(o.group)} · ` : ''}kept through every edit</span></div>${rows}${kind === 'distance' && targets.length ? `<label class="row small">Distance (m)<input id="c-distance" type="number" min="0" step="any" required value="${app.constraintDistance ?? 2}"></label>` : ''}${axes}${add}</div>`
}

function rigCard(o) {
  const head = (extra) => `<div class="card rig"><div class="card-title">Rig <span class="muted small">${extra}</span></div>`
  if (!o.bones?.length) {
    return `${head('bones · skinning')}<div class="row"><span class="muted small">Add a bone chain</span>${[2, 3, 4, 5, 6]
      .map((n) => `<button class="chip" data-rig-chain="${n}" title="${n} bones end to end through the mesh">${n}</button>`)
      .join('')}</div></div>`
  }
  const bone = currentBone(o)
  const k = o.bones.indexOf(bone)
  const r = boneRotations(o, app.frame)[k]
  const keyed = boneTrack(o, bone.name) ? ' <span class="keyed" title="Animated: edits key this frame">◆</span>' : ''
  const deg = (v) => Math.round((v * 180) / Math.PI)
  return `${head(`${o.bones.length} bones`)}
    <div class="brushes">${o.bones.map((b) => `<button class="chip ${b === bone ? 'on' : ''}" data-bone="${escapeHtml(b.name)}">${escapeHtml(b.name)}</button>`).join('')}</div>
    ${['X', 'Y', 'Z']
      .map((a, i) => `<label class="slider"><span>Turn ${a}${keyed}</span><input type="range" min="-180" max="180" step="1" id="p-bone-${i}" value="${deg(r[i])}"><b>${deg(r[i])}°</b></label>`)
      .join('')}
    <div class="row"><button class="small-btn ${app.ik ? 'on' : ''}" data-rig="ik" title="Drag a handle at the bone's tip; the chain bends to follow (inverse kinematics)">Reach (IK)</button><button class="small-btn" data-rig="key" title="Key every bone at this frame">◆ Key pose</button></div>
    <div class="row"><button class="small-btn" data-rig="reset">Reset pose</button><button class="small-btn" data-rig="remove">Remove rig</button></div>
  </div>`
}

/** The chosen bone's rotation from the three Turn sliders, in radians. */
function boneSliders() {
  return [0, 1, 2].map((i) => Math.round(((Number($(`p-bone-${i}`).value) * Math.PI) / 180) * 1e5) / 1e5)
}

/** Commands that set a bone's turn: keyed bones get a key at this frame. */
function poseCommands(o, name, rotation) {
  const frame = Math.round(app.frame * 100) / 100
  return boneTrack(o, name)
    ? [{ op: 'set_keyframe', id: o.id, property: 'bone', bone: name, frame, value: rotation }]
    : [{ op: 'pose', id: o.id, bone: name, rotation }]
}

function modifierField(m, name, raw) {
  const v = Number(raw)
  if (!Number.isFinite(v)) return null
  const next = structuredClone(m)
  if (name === 'angle') next.angle = (v * Math.PI) / 180
  else if (name.startsWith('offset.')) next.offset[Number(name.split('.')[1])] = v
  else if (name === 'count') next.count = Math.round(v)
  else next[name] = v
  return next
}

$('properties').addEventListener('input', (e) => {
  const t = e.target
  if (t.id === 'w-strength' || t.id === 'w-rotation') {
    // Preview the light while dragging; the change commits it.
    t.nextElementSibling.textContent = t.id === 'w-rotation' ? `${t.value}°` : fmt(Number(t.value))
    viewport.setWorld({ ...worldOf(), [t.id === 'w-strength' ? 'strength' : 'rotation']: Number(t.value) })
    return
  }
  const o = objectById(app.selected)
  if (!o || !/^p-bone-\d$/.test(e.target.id)) return
  // Show the turn while dragging; the change commits it.
  e.target.nextElementSibling.textContent = `${e.target.value}°`
  viewport.previewBone(o.id, currentBone(o).name, boneSliders())
})
$('properties').addEventListener('change', (e) => {
  const target = e.target
  if (target.id === 'w-strength') return run([{ op: 'world', strength: Number(target.value) }]).catch(() => {})
  if (target.id === 'w-rotation') return run([{ op: 'world', rotation: Number(target.value) }]).catch(() => {})
  if (target.id === 'w-bg') return run([{ op: 'world', background: target.checked }]).catch(() => {})
  const o = objectById(app.selected)
  if (!o) return
  if (/^p-bone-\d$/.test(target.id)) return run(poseCommands(o, currentBone(o).name, boneSliders())).catch(() => {})
  if (['light-kind','light-color','light-intensity'].includes(target.id)) {
    const field = target.id.slice(6)
    return run([{op:'light_settings',id:o.id,lamp:{...o.light,[field]:field==='intensity'?Number(target.value):target.value}}]).catch(() => {})
  }
  if (target.dataset.cameraLens) return run([{ op: 'camera_settings', id: o.id, lens: { ...o.camera, [target.dataset.cameraLens]: Number(target.value) } }]).catch(() => {})
  if (target.id === 'p-name') return run([{ op: 'rename', id: o.id, name: target.value }]).catch(() => {})
  if (target.id === 'p-dist') {
    app.extrudeDistance = Number(target.value) || 0.3
    return
  }
  if (target.id === 'p-bevel') {
    const w = Number(target.value)
    app.bevelWidth = w > 0 ? w : 0.1
    return
  }
  if (target.id === 'p-inset') {
    const f = Number(target.value)
    app.insetFraction = f > 0 && f < 1 ? f : 0.3
    return
  }
  if (target.dataset.mod !== undefined) {
    const i = Number(target.dataset.mod)
    const next = modifierField(o.modifiers[i], target.dataset.field, target.value)
    if (!next) return render()
    return actions.setModifier(i, next).catch(() => {})
  }
  if (target.id === 'c-kind') {
    app.constraintKind = target.value
    return render()
  }
  if (target.id === 'c-distance') {
    app.constraintDistance = Number(target.value)
    return
  }
  if (target.id === 'c-align') {
    app.constraintAxes = target.value
    return
  }
  if (target.matches('[data-layout-item]')) {
    app.layoutItems = [...document.querySelectorAll('[data-layout-item]:checked')].map((x) => JSON.parse(x.dataset.layoutItem))
    return
  }
  if (target.id === 'layout-kind') { app.layoutKind = target.value; return }
  if (target.id === 'layout-spacing') { app.layoutSpacing = Number(target.value); return }
  if (target.id === 'layout-radius') { app.layoutRadius = target.value; return }
  if (target.id === 'bool-with') {
    app.boolWith = Number(target.value)
    return render()
  }
  if (target.id === 'bool-keep') {
    app.boolKeep = target.checked
    return
  }
  if (target.id === 'b-radius' || target.id === 'b-strength' || target.id === 'b-detail') {
    app.brush = { ...app.brush, [{ 'b-radius': 'radius', 'b-strength': 'strength', 'b-detail': 'detail' }[target.id]]: Number(target.value) }
    return syncEdit()
  }
  if (target.id === 'p-color') return run(editCommands(o.id, { color: target.value })).catch(() => {})
  if (target.id === 'p-rough') return run(editCommands(o.id, { roughness: Number(target.value) })).catch(() => {})
  if (target.id === 'p-metal') return run(editCommands(o.id, { metalness: Number(target.value) })).catch(() => {})
  if (target.id === 'p-trans') return run(editCommands(o.id, { transmission: Number(target.value) })).catch(() => {})
  if (target.id === 'p-opacity') return run(editCommands(o.id, { opacity: Number(target.value) })).catch(() => {})
  if (target.id === 'p-emissive') {
    // A colour on its own would barely show, so give it a visible glow.
    const strength = pose(o, app.frame).material.emissive_strength
    return run(editCommands(o.id, { emissive: target.value, emissive_strength: strength > 0 ? undefined : 2 })).catch(() => {})
  }
  if (target.id === 'p-color2' || target.id === 'p-tscale' || target.id === 'p-relief') {
    const texture = { ...o.material.texture }
    if (target.id === 'p-color2') texture.color2 = target.value
    else if (target.id === 'p-relief') texture.relief = Number(target.value)
    else texture.scale = Number(target.value)
    return run([{ op: 'material', id: o.id, texture }]).catch(() => {})
  }
  if (target.id === 'p-emit') return run(editCommands(o.id, { emissive_strength: Number(target.value) })).catch(() => {})
  const field = target.dataset.field
  if (field) {
    const values = [...pose(o, app.frame).transform[field]]
    let v = Number(target.value)
    if (!Number.isFinite(v)) return render()
    if (field === 'rotation') v = (v * Math.PI) / 180
    values[Number(target.dataset.i)] = v
    run(editCommands(o.id, { [field]: values })).catch(() => {})
  }
})
$('properties').addEventListener('click', (e) => {
  const rigObject = objectById(app.selected)
  const chain = e.target.closest('[data-rig-chain]')
  if (chain && rigObject) return run([{ op: 'rig', id: rigObject.id, chain: Number(chain.dataset.rigChain) }]).catch(() => {})
  const boneChip = e.target.closest('[data-bone]')
  if (boneChip) {
    app.bone = boneChip.dataset.bone
    return syncEdit()
  }
  const rigAction = e.target.closest('[data-rig]')?.dataset.rig
  if (rigAction && rigObject?.bones?.length) {
    if (rigAction === 'ik') {
      app.ik = !app.ik
      return syncEdit()
    }
    if (rigAction === 'key') return keyBones(rigObject).catch(() => {})
    if (rigAction === 'reset') return run(rigObject.bones.flatMap((b) => poseCommands(rigObject, b.name, [0, 0, 0]))).catch(() => {})
    if (rigAction === 'remove') return run([{ op: 'rig', id: rigObject.id, bones: [] }]).catch(() => {})
  }
  const sky = e.target.closest('[data-sky]')
  if (sky) return run([{ op: 'world', sky: sky.dataset.sky }]).catch(() => {})
  const worldImage = e.target.closest('[data-world-image]')
  if (worldImage) return run([{ op: 'world', image: worldImage.dataset.worldImage }]).catch(() => {})
  if (e.target.closest('[data-world-hdri]')) return $('world-input').click()
  const build = e.target.closest('[data-build]')
  if (build) return run([{ op: 'build', template: build.dataset.build, translation: [freeSpot(0.7), 0, 0] }]).then((r) => r && select(r.created[0])).catch(() => {})
  const o0 = objectById(app.selected)
  if (e.target.closest('[data-keep-layout]') && o0) {
    if (!$('layout-spacing').reportValidity() || !$('layout-radius').reportValidity()) return
    const ids = [...document.querySelectorAll('[data-layout-item]:checked')].map((x) => JSON.parse(x.dataset.layoutItem))
    if (!ids.length) return toast('Choose at least one item to arrange.', 'error')
    const radius = $('layout-radius').value
    return run([{op: 'arrange', ids, layout: $('layout-kind').value, around: subjectOf(o0), spacing: Number($('layout-spacing').value), ...(radius === '' ? {} : {radius: Number(radius)}), keep: true}]).catch(() => {})
  }
  const unarrange = e.target.closest('[data-unarrange]')
  if (unarrange) return run([{op: 'unarrange', id: Number(unarrange.dataset.unarrange)}]).catch(() => {})
  if (e.target.closest('[data-constrain]') && o0) {
    const kind = $('c-kind').value
    const target = JSON.parse($('c-target').value)
    if (kind === 'distance' && !$('c-distance').reportValidity()) return
    const rule = kind === 'distance'
      ? { distance: Number($('c-distance').value), from: target }
      : kind === 'align' ? { align: $('c-align').value, from: target } : { [kind]: target }
    return run([{ op: 'constrain', id: subjectOf(o0), ...rule }]).catch(() => {})
  }
  const unconstrain = e.target.closest('[data-unconstrain]')
  if (unconstrain) {
    const [subject, kind] = JSON.parse(unconstrain.dataset.unconstrain)
    return run([{ op: 'unconstrain', id: subject, kind }]).catch(() => {})
  }
  const addMod = e.target.closest('[data-add-mod]')
  if (addMod) return actions.addModifier(addMod.dataset.addMod).catch(() => {})
  const remove = e.target.closest('[data-mod-remove]')
  if (remove) return actions.removeModifier(Number(remove.dataset.modRemove)).catch(() => {})
  if (e.target.closest('[data-mod-apply]')) return actions.applyModifiers().catch(() => {})
  const set = e.target.closest('[data-set]')
  if (set && o0) {
    const i = Number(set.dataset.mod)
    const next = { ...o0.modifiers[i] }
    next[set.dataset.set] = set.dataset.set === 'levels' ? Number(set.dataset.value) : set.dataset.value
    return actions.setModifier(i, next).catch(() => {})
  }
  const bool = e.target.closest('[data-bool]')
  if (bool && o0 && app.boolWith != null) {
    return run([{ op: 'boolean', id: o0.id, with: app.boolWith, operation: bool.dataset.bool, keep: app.boolKeep }]).catch(() => {})
  }
  const groupAct = e.target.closest('[data-group-act]')
  if (groupAct && o0?.group) {
    const id = o0.group
    const op = { turn: { op: 'move', id, rotate_y: Math.PI / 2 }, drop: { op: 'drop', id }, delete: { op: 'delete', id } }[groupAct.dataset.groupAct]
    return run([op]).catch(() => {})
  }
  if (e.target.closest('[data-drop]') && o0) return run([{ op: 'drop', id: o0.id }]).catch(() => {})
  if (e.target.closest('[data-shade]') && o0) return run([{ op: 'shade', id: o0.id, smooth: !o0.smooth }]).catch(() => {})
  const brush = e.target.closest('[data-brush]')
  if (brush) {
    app.brush = { ...app.brush, brush: brush.dataset.brush }
    return syncEdit()
  }
  const toggle = e.target.closest('[data-brush-toggle]')
  if (toggle) {
    const k = toggle.dataset.brushToggle
    app.brush = { ...app.brush, [k]: k === 'symmetry' ? (app.brush.symmetry ? null : 'x') : !app.brush[k] }
    return syncEdit()
  }
  if (e.target.closest('[data-tex-fit]') && o0?.material.texture) {
    return run([{ op: 'material', id: o0.id, texture: { ...o0.material.texture, fit: !o0.material.texture.fit } }]).catch(() => {})
  }
  if (e.target.closest('[data-edit-nodes]') && o0) return openNodes(o0)
  if (e.target.closest('[data-uv-open]') && o0) return openUv(o0)
  const seam = e.target.closest('[data-seam]')
  if (seam && o0 && app.sel.edges.length) {
    return run([{ op: 'mark_seams', id: o0.id, edges: app.sel.edges, clear: seam.dataset.seam === 'clear' }]).catch(() => {})
  }
  const pattern = e.target.closest('[data-pattern]')
  if (pattern && o0) {
    const [id, , color2] = PATTERNS.find(([p]) => p === pattern.dataset.pattern)
    if (id === 'image') {
      app.imageTarget = o0.id
      return $('image-input').click()
    }
    const old = o0.material.texture
    if (id === 'nodes') {
      if (old?.pattern === 'nodes') return openNodes(o0)
      const texture = { pattern: 'nodes', graph: starterGraph(o0.material.color), scale: old?.scale ?? 0.5, relief: old?.relief || 0.4 }
      return run([{ op: 'material', id: o0.id, texture }])
        .then(() => {
          const o = objectById(o0.id)
          if (o?.material.texture?.pattern === 'nodes') openNodes(o)
        })
        .catch(() => {})
    }
    const texture = id === 'none' ? { pattern: id } : { pattern: id, color2, scale: old?.scale ?? 0.5, relief: old?.relief ?? 0 }
    return run([{ op: 'material', id: o0.id, texture }]).catch(() => {})
  }
  const preset = e.target.closest('[data-preset]')
  if (preset && o0) return run([{ op: 'material', id: o0.id, preset: preset.dataset.preset }]).catch(() => {})
  const g = e.target.closest('[data-glaze]')
  const o = objectById(app.selected)
  if (!g || !o) return
  const [color, roughness, metalness] = g.dataset.glaze.split(',')
  // Glazes are opaque ceramics: they also clear glass and glow.
  run(editCommands(o.id, { color, roughness: Number(roughness), metalness: Number(metalness), transmission: 0, emissive: '#000000' })).catch(() => {})
})

function renderActivity() {
  $('activity').innerHTML = app.activity
    .map((a) => `<li><span class="src src-${a.source.split(' ')[0].toLowerCase()}">${escapeHtml(a.source)}</span><span class="what">${escapeHtml(a.text)}</span><span class="rev">r${a.revision}</span></li>`)
    .join('')
}

$('batch-input').value = JSON.stringify(
  { commands: [{ op: 'add', name: 'Bowl', primitive: { kind: 'vessel', profile: [[0.12, 0], [0.3, 0.12], [0.36, 0.24]] }, color: '#8fb9a0', roughness: 0.22 }] },
  null,
  2,
)
$('batch-apply').addEventListener('click', async () => {
  let batch
  try {
    batch = JSON.parse($('batch-input').value)
  } catch (e) {
    return toast(`Invalid JSON: ${e.message}`, 'error')
  }
  const commands = Array.isArray(batch) ? batch : batch.commands
  if (!Array.isArray(commands)) return toast('Expected {"commands": [...]}', 'error')
  run(commands, 'Batch').catch(() => {})
})
$('chat-send').addEventListener('click', sendChat)
/** Show an inspection report in the Checks card and outline flagged objects. */
function showChecks(report) {
  app.checks = report
  const ids = new Map(report.objects.map((o) => [o.name, o.id]))
  for (const o of report.objects) if (o.group && !ids.has(o.group)) ids.set(o.group, o.id)
  const flagged = new Set()
  const items = report.issues.map((issue) => {
    const names = issue.objects || [issue.object]
    for (const n of names) if (ids.has(n)) flagged.add(ids.get(n))
    const level = issue.kind === 'floating' ? 'info' : 'warn'
    return `<li class="issue ${level}" data-issue-id="${ids.get(names[0]) ?? ''}">${escapeHtml(issue.message)}</li>`
  })
  $('checks-out').innerHTML =
    `<div class="checks-summary ${report.issues.length ? 'bad' : 'good'}">${report.issues.length ? '⚠' : '✓'} ${escapeHtml(report.summary)} <span class="muted">r${report.revision}</span></div>` +
    (items.length ? `<ul class="issues">${items.join('')}</ul>` : '')
  viewport.setWarnings([...flagged])
}
app.showChecks = showChecks

$('inspect-btn').addEventListener('click', () =>
  track(async () => showChecks(await api.inspect())).catch((e) => toast(e.message, 'error')),
)
$('checks-out').addEventListener('click', (e) => {
  const li = e.target.closest('[data-issue-id]')
  if (li && li.dataset.issueId) select(Number(li.dataset.issueId))
})

async function agentRender(samples) {
  const query = `views=iso,front,right,top&size=256&frame=${Math.round(app.frame)}${samples ? `&samples=${samples}` : ''}`
  const res = await clock.track(fetch(`/api/render?${query}`))
  if (!res.ok) return toast('Render failed', 'error')
  const img = new Image()
  img.alt = `Front, right, top and iso views ${samples ? 'path traced' : 'rendered'} by the Rust engine`
  img.src = URL.createObjectURL(await clock.track(res.blob()))
  $('render-out').replaceChildren(img)
}
$('render-btn').addEventListener('click', () => agentRender(0))
// Path tracing in the browser build shares the page's thread: fewer samples.
$('render-pt-btn').addEventListener('click', () => agentRender(browserOnly ? 4 : 16))
$('chat-input').addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) sendChat()
})
async function sendChat() {
  const prompt = $('chat-input').value.trim()
  if (!prompt || !app.ai || chatBusy) return
  chatBusy = true
  $('chat-review').disabled = true
  $('chat-status').textContent = 'Thinking…'
  $('chat-send').disabled = true
  try {
    const r = await track(async () => {
      const res = await api.chat(prompt, $('chat-review').checked ? 'proposal' : 'apply')
      await refresh(true)
      return res
    })
    log('Chat', `${r.mode === 'proposal' ? 'proposed · ' : ''}${summarize(r.commands)}`, r.revision)
    $('batch-input').value = JSON.stringify({ commands: r.commands }, null, 2)
    $('chat-status').textContent = r.mode === 'proposal' ? `Ready to review · ${r.commands.length} command${r.commands.length === 1 ? '' : 's'}` : `Applied ${r.commands.length} command${r.commands.length === 1 ? '' : 's'}`
    $('chat-input').value = ''
  } catch (e) {
    $('chat-status').textContent = ''
    toast(e.message, 'error')
  } finally {
    chatBusy = false
    $('chat-review').disabled = false
    $('chat-send').disabled = !app.ai
  }
}

function toast(text, kind = 'info') {
  const el = document.createElement('div')
  el.className = `toast ${kind}`
  el.textContent = text
  $('toasts').append(el)
  setTimeout(() => el.remove(), kind === 'error' ? 5000 : 2600)
}
app.toast = toast

document.addEventListener('keydown', (e) => {
  if (e.target.closest('input, textarea')) return
  const mod = e.ctrlKey || e.metaKey
  const k = e.key.toLowerCase()
  if (mod && k === 'z') {
    e.preventDefault()
    return e.shiftKey ? actions.redo() : actions.undo()
  }
  if (mod && k === 'y') return actions.redo()
  if (k === 'f12') {
    e.preventDefault()
    return actions.renderImage()
  }
  if (mod && k === 'b') {
    e.preventDefault()
    return actions.bevel()?.catch?.(() => {})
  }
  if (mod && k === 'r') {
    e.preventDefault()
    return actions.loopCut()?.catch?.(() => {})
  }
  if (mod && k === 's') {
    e.preventDefault()
    return actions.save()
  }
  if (mod) return
  if (k === ' ') {
    e.preventDefault()
    return togglePlay()
  }
  if (k === 'k') return keyAll()?.catch?.(() => {})
  if (k === 'arrowright' || k === 'arrowleft') {
    e.preventDefault()
    return setFrame(Math.round(app.frame) + (k === 'arrowright' ? 1 : -1))
  }
  if (k === 'tab') {
    e.preventDefault()
    return actions.editMode()
  }
  if (app.mode === 'edit' && ['1', '2', '3'].includes(k)) return setSelectMode(['vertex', 'edge', 'face'][Number(k) - 1])
  if (app.mode === 'edit' && k === 'a') return selectAll()
  if (app.mode === 'sculpt' && (k === '[' || k === ']')) {
    const radius = Math.min(1, Math.max(0.02, app.brush.radius * (k === ']' ? 1.25 : 0.8)))
    app.brush = { ...app.brush, radius: Math.round(radius * 1000) / 1000 }
    return syncEdit()
  }
  if (k === 'g') viewport.setGizmoMode('translate')
  else if (k === 'r') viewport.setGizmoMode('rotate')
  else if (k === 's') viewport.setGizmoMode('scale')
  else if (k === 'f') actions.frame()
  else if (k === 'w') actions.wireframe()
  else if (k === 'z') actions.rendered()
  else if (k === 'e') actions.extrude()?.catch?.(() => {})
  else if (k === 'i') actions.inset()?.catch?.(() => {})
  else if (k === 'd' && e.shiftKey) actions.duplicate()?.catch?.(() => {})
  else if ((k === 'delete' || k === 'backspace' || k === 'x') && app.mode === 'object') actions.delete()?.catch?.(() => {})
  else if (k === 'escape') {
    if (viewport.sceneCameraId != null) { viewport.lookThrough(null); renderedKey = ''; return render() }
    if (app.mode !== 'object') setMode('object')
    else select(null)
  }
})

// ---------------------------------------------------------------------------
// Demo replay and capture hooks
// ---------------------------------------------------------------------------

const demo = new DemoRunner(app)
$('demo-btn').addEventListener('click', async () => {
  if (demo.running) return
  $('demo-btn').disabled = true
  try {
    await demo.play(SCENARIOS.hero)
  } catch (e) {
    toast(e.message, 'error')
  } finally {
    $('demo-btn').disabled = false
  }
})

// ---------------------------------------------------------------------------
// Animation: timeline, playback and auto-key
// ---------------------------------------------------------------------------

const animRange = () => app.scene.animation || { start: 1, end: 96, fps: 24 }

/**
 * Commands for editing transform/material values. Properties that already
 * have a track are keyed at the current frame instead (auto-key), so edits on
 * animated objects change the animation rather than a hidden static value.
 */
function editCommands(id, changes) {
  const o = objectById(id)
  const frame = Math.round(app.frame * 100) / 100
  const commands = []
  const transform = {}
  const material = {}
  for (const [p, v] of Object.entries(changes)) {
    if (v === undefined) continue
    if (o && trackOf(o, p)) commands.push({ op: 'set_keyframe', id, property: p, frame, value: v })
    else if (['translation', 'rotation', 'scale'].includes(p)) transform[p] = v
    else material[p] = v
  }
  if (Object.keys(transform).length) commands.unshift({ op: 'transform', id, ...transform })
  if (Object.keys(material).length) commands.unshift({ op: 'material', id, ...material })
  return commands
}

function setFrame(f, { fromPlayback = false } = {}) {
  const { start, end } = animRange()
  app.frame = Math.min(end, Math.max(start, f))
  if (!fromPlayback) app.playFrom = app.playing ? { t: clock.now(), f: app.frame } : null
  viewport.setFrame(app.frame)
  renderTimeline()
  if (!app.playing) render()
}

function togglePlay(on = !app.playing) {
  app.playing = on
  app.playFrom = on ? { t: clock.now(), f: app.frame } : null
  if (!on) setFrame(Math.round(app.frame))
  render()
}

function advancePlayback() {
  if (!app.playing || !app.playFrom) return
  const { start, end, fps } = animRange()
  const span = end - start
  const f = app.playFrom.f - start + ((clock.now() - app.playFrom.t) / 1000) * fps
  setFrame(start + (((f % span) + span) % span), { fromPlayback: true })
}

function frameToX(f) {
  const { start, end } = animRange()
  return ((f - start) / (end - start)) * 100
}

let ticksFor = ''
function renderTimeline() {
  const { start, end, fps } = animRange()
  const key = `${start}:${end}`
  if (key !== ticksFor) {
    ticksFor = key
    const step = end - start > 200 ? 48 : end - start > 60 ? 12 : 6
    const ticks = []
    for (let f = Math.ceil(start / step) * step; f <= end; f += step) ticks.push(`<span style="left:${frameToX(f)}%">${f}</span>`)
    $('ruler-ticks').innerHTML = ticks.join('')
  }
  $('playhead').style.left = `${frameToX(app.frame)}%`
  if (document.activeElement !== $('frame-input')) $('frame-input').value = String(Math.round(app.frame))
  $('play-btn').innerHTML = icon(app.playing ? 'pause' : 'play')
  $('play-btn').classList.toggle('on', app.playing)
  $('range-label').textContent = `${start}–${end} · ${fps} fps`
  const sel = objectById(app.selected)
  const keys = sel
    ? keyFrames(sel).map((f) => `<i style="left:${frameToX(f)}%"></i>`)
    : [...new Set(app.scene.objects.flatMap(keyFrames))].map((f) => `<i class="faint" style="left:${frameToX(f)}%"></i>`)
  const html = keys.join('')
  if ($('ruler-keys').innerHTML !== html) $('ruler-keys').innerHTML = html
  $('key-btn').disabled = !sel
}

function keyAll() {
  const o = objectById(app.selected)
  if (!o) return toast('Select an object to key')
  const frame = Math.round(app.frame)
  const bones = (o.bones || []).map((b) => ({ op: 'set_keyframe', id: o.id, property: 'bone', bone: b.name, frame }))
  return run([...PROPERTIES.map((property) => ({ op: 'set_keyframe', id: o.id, property, frame })), ...bones], 'UI')
}

/** Key every bone's current turn at this frame. */
function keyBones(o) {
  const frame = Math.round(app.frame)
  return run(o.bones.map((b) => ({ op: 'set_keyframe', id: o.id, property: 'bone', bone: b.name, frame })), 'UI')
}

$('play-btn').addEventListener('click', () => togglePlay())
$('key-btn').addEventListener('click', () => keyAll()?.catch?.(() => {}))
$('frame-input').addEventListener('change', (e) => {
  const f = Number(e.target.value)
  if (Number.isFinite(f)) setFrame(f)
  e.target.blur()
})
{
  let scrubbing = false
  const scrub = (e) => {
    const r = $('ruler').getBoundingClientRect()
    const { start, end } = animRange()
    setFrame(Math.round(start + ((e.clientX - r.left) / r.width) * (end - start)))
  }
  $('ruler').addEventListener('pointerdown', (e) => {
    scrubbing = true
    $('ruler').setPointerCapture(e.pointerId)
    if (app.playing) togglePlay(false)
    scrub(e)
  })
  $('ruler').addEventListener('pointermove', (e) => scrubbing && scrub(e))
  $('ruler').addEventListener('pointerup', () => (scrubbing = false))
}
app.setFrame = setFrame
app.togglePlay = togglePlay
app.frameToX = frameToX
app.keyAll = keyAll

/** One display frame: tweens, animation playback, then draw. */
function stepFrame() {
  animator.step()
  advancePlayback()
  viewport.frame()
  preview.tick()
}

function loop() {
  stepFrame()
  requestAnimationFrame(loop)
}

const status = { done: false, error: null, started: false }
const ready = (async () => {
  if (browserOnly) {
    await installWasmBackend(`${import.meta.env.BASE_URL}tatara.wasm`)
    $('status-hint').dataset.static = '1'
  }
  await refresh(false)
  setFrame(app.scene.animation?.start ?? 1)
  if (app.scene.objects.length) viewport.frameAll(0)
  stepFrame()
  if (!capture) requestAnimationFrame(loop)
  if (!browserOnly) {
    collaboration = new Collaboration({ api, viewport, scene: () => app.scene, selected: () => app.selected, onIdle: () => catchUp().catch(() => {}), capture })
    app.collaboration = collaboration
    api.setActor(collaboration.actor)
    if (!capture) await collaboration.send(true)
  }
  if (!capture && !browserOnly) {
    const events = new EventSource('/api/events')
    events.addEventListener('open', () => {
      connectionEpoch++
      allowRevisionReset = true
      remoteRevision = Infinity
      collaboration.reset()
      catchUp().catch(() => {})
      collaboration.send(true)
      collaboration.load()
    })
    events.addEventListener('presence', () => collaboration.load())
    events.addEventListener('resync', () => { remoteRevision = Infinity; catchUp().catch(() => {}); collaboration.load() })
    events.addEventListener('proposals', (e) => {
      if (Number(e.data) === proposalTray.version) return
      api.proposals().then((p) => proposalTray.set(p, app.scene.revision)).catch(() => {})
    })
    events.addEventListener('revision', (e) => {
      const rev = Number(e.data)
      remoteRevision = Math.max(remoteRevision === Infinity ? 0 : remoteRevision, rev)
      catchUp().catch(() => {})
    })
  }
})()

window.__tatara = {
  ready,
  scenarios: Object.keys(SCENARIOS),
  presence: () => collaboration?.peers ?? [],
  actor: () => collaboration?.actor ?? null,
  selection: () => ({ id: app.selected, face: app.face }),
  frame: () => app.frame,
  position: (id) => viewport.nodes.get(id)?.group.position.toArray(),
  camera: () => viewport.getOrbit(),
  refresh: (animate = false) => refresh(animate),
  meta: (id) => {
    const s = SCENARIOS[id]
    return s && { id, title: s.title, width: s.width || 800, external: Boolean(s.external) }
  },
  play(id) {
    const s = SCENARIOS[id]
    if (!s) throw new Error(`unknown scenario ${id}`)
    status.started = true
    demo
      .play(s)
      .then(() => (status.done = true))
      .catch((e) => {
        status.error = String(e?.stack || e)
        status.done = true
      })
  },
  debug: () => ({ sceneLights: [...viewport.nodes.values()].filter(n=>n.light?.visible).length, sceneCamera: viewport.sceneCameraId ?? null, renderCamera: { eye: viewport.camera.position.toArray(), up: viewport.camera.up.toArray(), fov: viewport.camera.fov }, pending: clock.pending, timers: clock.timers.length, now: clock.now(), anims: [...animator.items.keys()], textured: [...viewport.nodes.values()].filter((n) => n.mesh.material.map?.image).length,
    normalMapped: [...viewport.nodes.values()].filter((n) => n.mesh.material.normalMap?.image).length,
    pathSamples: preview.active ? preview.samples : null,
    finalSamples: finalRender.open ? finalRender.samples : null,
    world: viewport.worldMap?.source ?? null,
    proposals: proposalTray.pending.length,
    previewing: app.previewing,
    worldBackground: Boolean(viewport.scene.background),
    bonesShown: [...viewport.nodes.values()].filter((n) => n.bones.visible).length,
    reachHandle: viewport.ikHandle.visible,
    triplanar: [...viewport.nodes.values()].filter((n) => 'TRIPLANAR' in n.mesh.material.defines).length,
    roughnessMapped: [...viewport.nodes.values()].filter((n) => n.mesh.material.roughnessMap?.image).length,
  }),
  async tick(ms) {
    clock.advance(ms)
    await clock.settle()
    stepFrame()
    // Wait for the GPU (reading a pixel blocks until the frame is drawn), so
    // heavy frames (glass, bloom) cannot pile up behind a caller that does
    // not screenshot every tick.
    const gl = viewport.renderer.getContext()
    gl.readPixels(0, 0, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array(4))
    return { done: status.done, error: status.error, time: clock.now() }
  },
  /** Start a tick without returning a promise; poll `tickResult` for completion. */
  startTick(ms) {
    window.__tatara.tickResult = null
    window.__tatara.tick(ms).then(
      (r) => (window.__tatara.tickResult = r),
      (e) => (window.__tatara.tickResult = { done: true, error: String(e?.stack || e) }),
    )
  },
  tickResult: null,
}
