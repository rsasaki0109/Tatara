import './style.css'
import { createApi } from './api.js'
import { Animator, Clock } from './clock.js'
import { Viewport } from './viewport.js'
import { DemoRunner } from './demo.js'
import { SCENARIOS } from './scenarios.js'
import { icon } from './icons.js'

const params = new URLSearchParams(location.search)
const capture = params.has('capture')
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
}

const viewport = new Viewport($('viewport'), clock, animator, {
  capture,
  onPick: (hit, e) => {
    if (!hit) return select(null)
    if (e.altKey) select(hit.id, hit.face)
    else select(hit.id)
  },
  onTransform: (id, tf) => run([{ op: 'transform', id, ...tf }], 'Gizmo').catch(() => {}),
})
app.viewport = viewport

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

function objectById(id) {
  return app.scene.objects.find((o) => o.id === id)
}

async function refresh(animate = true) {
  const st = await api.state()
  app.scene = st.scene
  app.history = st.history
  app.ai = st.ai
  if (app.selected != null && !objectById(app.selected)) {
    app.selected = null
    app.face = null
  }
  const sel = objectById(app.selected)
  if (sel && app.face != null && app.face >= sel.mesh.faces.length) app.face = null
  viewport.sync(app.scene, animate)
  viewport.setSelection(app.selected, app.face)
  render()
}

function summarize(commands) {
  const parts = []
  for (const c of commands) {
    const label = c.op === 'add' ? `add ${c.primitive.kind}` : c.op
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
  }
}

/** Apply a batch through the shared command API. */
async function run(commands, source = 'UI') {
  return track(async () => {
    try {
      const r = await api.commands(commands, app.scene.revision)
      await refresh(true)
      log(source, summarize(commands), r.revision)
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
  app.selected = id
  app.face = id == null ? null : face
  viewport.setSelection(app.selected, app.face)
  render()
}

function freeSpot(half) {
  const box = viewport.sceneBounds()
  if (box.isEmpty()) return 0
  return Math.round((box.max.x + 0.35 + half) * 100) / 100
}

const actions = {
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
    if (app.selected == null || app.face == null) return toast('Alt+click a face to extrude it')
    return run([{ op: 'extrude', id: app.selected, face: app.face, distance }])
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
  wireframe(on = !app.wireframe) {
    app.wireframe = on
    viewport.setWireframe(on)
    render()
  },
  frame: () => viewport.frameAll(),
}
app.actions = actions
app.run = run
app.select = select
app.refresh = refresh
app.log = log
app.summarize = summarize
app.render = () => render()

function download(href, name) {
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
    .join('')
$('edit-group').innerHTML =
  button('extrude', 'Extrude', 'Extrude selected face (E)') +
  button('subdivide', 'Subdivide', 'Catmull-Clark subdivision') +
  button('duplicate', 'Duplicate', 'Duplicate (Shift+D)') +
  button('delete', 'Delete', 'Delete (X)')
$('history-group').innerHTML =
  button('undo', '', 'Undo (Ctrl+Z)', 'icon-only') + button('redo', '', 'Redo (Ctrl+Shift+Z)', 'icon-only')
$('file-group').innerHTML =
  button('open', 'Open', 'Open .tatara.json') + button('save', 'Save', 'Save (Ctrl+S)') + button('exportObj', 'OBJ', 'Export OBJ')
$('wire-btn').innerHTML = icon('wire')
$('frame-btn').innerHTML = icon('frame')

document.addEventListener('click', (e) => {
  const add = e.target.closest('[data-add]')
  if (add) return actions.add(add.dataset.add).catch(() => {})
  const act = e.target.closest('[data-action]')
  if (act) {
    const r = actions[act.dataset.action]()
    if (r?.catch) r.catch(() => {})
  }
})
$('wire-btn').addEventListener('click', () => actions.wireframe())
$('frame-btn').addEventListener('click', () => actions.frame())

for (const tab of document.querySelectorAll('.tabs button')) tab.addEventListener('click', () => showTab(tab.dataset.tab))
function showTab(name) {
  for (const t of document.querySelectorAll('.tabs button')) t.classList.toggle('active', t.dataset.tab === name)
  for (const p of document.querySelectorAll('.tab-panel')) p.classList.toggle('active', p.dataset.panel === name)
}
app.showTab = showTab

const KIND_ICON = { cube: 'cube', sphere: 'sphere', cylinder: 'cylinder', torus: 'torus', vessel: 'vessel', plane: 'plane' }

function render() {
  const { objects } = app.scene
  const faces = objects.reduce((n, o) => n + o.mesh.faces.length, 0)
  $('object-count').textContent = objects.length ? String(objects.length) : ''
  $('outliner-empty').style.display = objects.length ? 'none' : ''
  $('outliner').innerHTML = objects
    .map(
      (o) =>
        `<li data-id="${o.id}" class="${o.id === app.selected ? 'selected' : ''}">${icon(KIND_ICON[o.kind] || 'cube')}<span class="name">${escapeHtml(o.name)}</span><span class="swatch" style="background:${o.material.color}"></span></li>`,
    )
    .join('')
  $('status-rev').textContent = `Revision ${app.scene.revision}`
  $('status-mesh').textContent = `${objects.length} object${objects.length === 1 ? '' : 's'} · ${faces.toLocaleString('en-US')} faces`
  for (const b of document.querySelectorAll('[data-action=undo]')) b.disabled = !app.history.can_undo
  for (const b of document.querySelectorAll('[data-action=redo]')) b.disabled = !app.history.can_redo
  const sel = objectById(app.selected)
  for (const a of ['duplicate', 'delete', 'subdivide']) document.querySelector(`[data-action=${a}]`).disabled = !sel
  document.querySelector('[data-action=extrude]').disabled = !sel || app.face == null
  $('wire-btn').classList.toggle('on', app.wireframe)
  $('ai-badge').textContent = app.replaying ? 'replay' : app.ai ? 'on' : 'off'
  $('ai-badge').classList.toggle('on', app.ai || Boolean(app.replaying))
  $('chat-input').disabled = !app.ai && !app.replaying
  $('chat-send').disabled = !app.ai
  $('chat-status').textContent = app.ai || app.replaying ? $('chat-status').textContent : 'Set TATARA_AI_* on the server to enable'
  renderProperties(sel)
}

$('outliner').addEventListener('click', (e) => {
  const li = e.target.closest('li[data-id]')
  if (li) select(Number(li.dataset.id))
})

function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c])
}

const fmt = (v, d = 2) => (Math.round(v * 10 ** d) / 10 ** d).toString()
let renderedKey = ''

function renderProperties(o) {
  const el = $('properties')
  const key = o ? JSON.stringify([o.id, o.name, o.transform, o.material, o.mesh.faces.length, app.face]) : 'none'
  if (key === renderedKey) return
  if (o && el.contains(document.activeElement) && document.activeElement.tagName === 'INPUT' && renderedKey.startsWith(`[${o.id},`)) return
  renderedKey = key
  if (!o) {
    el.innerHTML = `<div class="empty-props">${icon('cursor')}<p>Select an object in the viewport or outliner.</p><p class="muted">Alt+click selects a single face for extrusion.</p></div>`
    return
  }
  const t = o.transform
  const vec = (field, values, step, conv = (x) => x) =>
    values
      .map((v, i) => `<label class="axis axis-${'xyz'[i]}"><i>${'XYZ'[i]}</i><input type="number" step="${step}" data-field="${field}" data-i="${i}" value="${fmt(conv(v), 3)}"></label>`)
      .join('')
  const glazes = GLAZES.map(
    ([name, color, rough, metal = 0]) =>
      `<button class="glaze ${o.material.color === color ? 'on' : ''}" data-glaze="${color},${rough},${metal}" title="${name}" style="--c:${color}"></button>`,
  ).join('')
  el.innerHTML = `
    <div class="card">
      <div class="card-title">Object <span class="muted small">#${o.id} · ${o.kind}</span></div>
      <input class="name-input" id="p-name" value="${escapeHtml(o.name)}" spellcheck="false">
      <div class="meta">${o.mesh.vertices.length.toLocaleString('en-US')} vertices · ${o.mesh.faces.length.toLocaleString('en-US')} faces</div>
    </div>
    <div class="card">
      <div class="card-title">Transform</div>
      <div class="vec-row"><span>Location</span>${vec('translation', t.translation, 0.1)}</div>
      <div class="vec-row"><span>Rotation°</span>${vec('rotation', t.rotation, 5, (r) => (r * 180) / Math.PI)}</div>
      <div class="vec-row"><span>Scale</span>${vec('scale', t.scale, 0.1)}</div>
    </div>
    <div class="card">
      <div class="card-title">Material</div>
      <div class="glazes">${glazes}</div>
      <div class="row"><input type="color" id="p-color" value="${o.material.color}"><code class="muted">${o.material.color}</code></div>
      <label class="slider"><span>Roughness</span><input type="range" min="0" max="1" step="0.01" id="p-rough" value="${o.material.roughness}"><b>${fmt(o.material.roughness)}</b></label>
      <label class="slider"><span>Metalness</span><input type="range" min="0" max="1" step="0.01" id="p-metal" value="${o.material.metalness}"><b>${fmt(o.material.metalness)}</b></label>
    </div>
    <div class="card">
      <div class="card-title">Mesh</div>
      <div class="face-info ${app.face != null ? 'on' : ''}">${app.face != null ? `Face <b>${app.face}</b> selected` : 'Alt+click a face to select it'}</div>
      <div class="row">
        <label class="inline">Distance <input type="text" inputmode="decimal" id="p-dist" value="${app.extrudeDistance}"></label>
        <button class="small-btn" data-action="extrude" ${app.face == null ? 'disabled' : ''}>${icon('extrude')}Extrude</button>
      </div>
      <div class="row"><button class="small-btn wide" data-action="subdivide">${icon('subdivide')}Subdivide</button><button class="small-btn wide" data-action="duplicate">${icon('duplicate')}Duplicate</button></div>
    </div>`
}

$('properties').addEventListener('change', (e) => {
  const o = objectById(app.selected)
  if (!o) return
  const target = e.target
  if (target.id === 'p-name') return run([{ op: 'rename', id: o.id, name: target.value }]).catch(() => {})
  if (target.id === 'p-dist') {
    app.extrudeDistance = Number(target.value) || 0.3
    return
  }
  if (target.id === 'p-color') return run([{ op: 'material', id: o.id, color: target.value }]).catch(() => {})
  if (target.id === 'p-rough') return run([{ op: 'material', id: o.id, roughness: Number(target.value) }]).catch(() => {})
  if (target.id === 'p-metal') return run([{ op: 'material', id: o.id, metalness: Number(target.value) }]).catch(() => {})
  const field = target.dataset.field
  if (field) {
    const values = [...o.transform[field]]
    let v = Number(target.value)
    if (!Number.isFinite(v)) return render()
    if (field === 'rotation') v = (v * Math.PI) / 180
    values[Number(target.dataset.i)] = v
    run([{ op: 'transform', id: o.id, [field]: values }]).catch(() => {})
  }
})
$('properties').addEventListener('click', (e) => {
  const g = e.target.closest('[data-glaze]')
  const o = objectById(app.selected)
  if (!g || !o) return
  const [color, roughness, metalness] = g.dataset.glaze.split(',')
  run([{ op: 'material', id: o.id, color, roughness: Number(roughness), metalness: Number(metalness) }]).catch(() => {})
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
$('chat-input').addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) sendChat()
})
async function sendChat() {
  const prompt = $('chat-input').value.trim()
  if (!prompt || !app.ai) return
  $('chat-status').textContent = 'Thinking…'
  $('chat-send').disabled = true
  try {
    const r = await track(async () => {
      const res = await api.chat(prompt)
      await refresh(true)
      return res
    })
    log('Chat', summarize(r.commands), r.revision)
    $('batch-input').value = JSON.stringify({ commands: r.commands }, null, 2)
    $('chat-status').textContent = `Applied ${r.commands.length} command${r.commands.length === 1 ? '' : 's'}`
    $('chat-input').value = ''
  } catch (e) {
    $('chat-status').textContent = ''
    toast(e.message, 'error')
  } finally {
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
  if (mod && k === 's') {
    e.preventDefault()
    return actions.save()
  }
  if (mod) return
  if (k === 'g') viewport.setGizmoMode('translate')
  else if (k === 'r') viewport.setGizmoMode('rotate')
  else if (k === 's') viewport.setGizmoMode('scale')
  else if (k === 'f') actions.frame()
  else if (k === 'w') actions.wireframe()
  else if (k === 'e') actions.extrude()?.catch?.(() => {})
  else if (k === 'd' && e.shiftKey) actions.duplicate()?.catch?.(() => {})
  else if (k === 'delete' || k === 'backspace' || k === 'x') actions.delete()?.catch?.(() => {})
  else if (k === 'escape') select(null)
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

function loop() {
  animator.step()
  viewport.frame()
  requestAnimationFrame(loop)
}

const status = { done: false, error: null, started: false }
const ready = (async () => {
  await refresh(false)
  if (app.scene.objects.length) viewport.frameAll(0)
  animator.step()
  viewport.frame()
  if (!capture) {
    requestAnimationFrame(loop)
    const events = new EventSource('/api/events')
    events.addEventListener('revision', (e) => {
      const rev = Number(e.data)
      if (rev !== app.scene.revision && app.inflight === 0) {
        refresh(true).then(() => log('External', 'scene updated (MCP/API)', rev))
      }
    })
  }
})()

window.__tatara = {
  ready,
  scenarios: Object.keys(SCENARIOS),
  selection: () => ({ id: app.selected, face: app.face }),
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
  debug: () => ({ pending: clock.pending, timers: clock.timers.length, now: clock.now(), anims: [...animator.items.keys()] }),
  async tick(ms) {
    clock.advance(ms)
    await clock.settle()
    animator.step()
    viewport.frame()
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
