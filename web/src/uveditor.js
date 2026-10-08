// UV editor: the selected object's faces laid out in the unit square over a
// UV grid. Click an island to select it, drag to move it, and turn, scale
// or fill it from the toolbar; every edit is one `transform_uvs` command.

const SIZE = 300

/** Blender-style UV grid: coloured cells labelled A1-H8, v up. */
export function uvGridCanvas(size = 512) {
  const c = document.createElement('canvas')
  c.width = c.height = size
  const g = c.getContext('2d')
  const n = 8
  const cell = size / n
  const hues = [0, 30, 55, 120, 170, 200, 240, 290]
  for (let row = 0; row < n; row++) {
    for (let col = 0; col < n; col++) {
      const light = (row + col) % 2 === 0
      g.fillStyle = `hsl(${hues[(row + col * 3) % n]}, ${light ? 55 : 45}%, ${light ? 62 : 42}%)`
      g.fillRect(col * cell, row * cell, cell, cell)
      g.fillStyle = 'rgba(255,255,255,0.9)'
      g.font = `bold ${cell * 0.32}px sans-serif`
      g.textAlign = 'center'
      g.textBaseline = 'middle'
      // Row 0 is the top of the image, where v = 1.
      g.fillText(`${'ABCDEFGH'[col]}${n - row}`, col * cell + cell / 2, row * cell + cell / 2)
    }
  }
  g.strokeStyle = 'rgba(0,0,0,0.35)'
  g.lineWidth = 2
  for (let k = 0; k <= n; k++) {
    g.beginPath()
    g.moveTo(k * cell, 0)
    g.lineTo(k * cell, size)
    g.moveTo(0, k * cell)
    g.lineTo(size, k * cell)
    g.stroke()
  }
  return c
}

/** Faces whose UVs touch along a shared edge form one island. */
export function uvIslands(mesh) {
  const parent = mesh.faces.map((_, i) => i)
  const root = (i) => {
    while (parent[i] !== i) i = parent[i] = parent[parent[i]]
    return i
  }
  const seen = new Map()
  mesh.faces.forEach((f, fi) => {
    for (let k = 0; k < f.length; k++) {
      const k2 = (k + 1) % f.length
      const [a, b] = [f[k], f[k2]]
      const [ua, ub] = [mesh.uvs[fi][k], mesh.uvs[fi][k2]]
      const key = a < b ? `${a},${b}` : `${b},${a}`
      const uvKey = (a < b ? [ua, ub] : [ub, ua]).map((p) => p.map((x) => x.toFixed(6)).join(':')).join('|')
      const other = seen.get(key)
      if (other && other.uvKey === uvKey) parent[root(fi)] = root(other.fi)
      else if (!other) seen.set(key, { fi, uvKey })
    }
  })
  const groups = new Map()
  mesh.faces.forEach((_, fi) => {
    const r = root(fi)
    if (!groups.has(r)) groups.set(r, [])
    groups.get(r).push(fi)
  })
  return [...groups.values()]
}

const hasUvs = (m) => Boolean(m.uvs?.length) && m.uvs.length === m.faces.length

export class UvEditor {
  /**
   * `onTransform(faces, {offset, rotate, scale})` moves UVs of `faces`;
   * `onUnwrap(method)` unwraps; `onClose()` closes the panel.
   */
  constructor(el, { onTransform, onUnwrap, onClose }) {
    this.el = el
    this.onTransform = onTransform
    this.onUnwrap = onUnwrap
    this.onClose = onClose
    this.mesh = null
    this.islands = []
    this.selected = -1
    this.drag = null
    this.grid = uvGridCanvas(256)
    el.addEventListener('click', (e) => this.click(e))
    el.addEventListener('pointerdown', (e) => this.down(e))
    window.addEventListener('pointermove', (e) => this.move(e))
    window.addEventListener('pointerup', (e) => this.up(e))
  }

  get open() {
    return !this.el.hidden
  }

  show(object) {
    this.el.hidden = false
    this.objectId = object.id
    this.selected = -1
    this.render(object)
  }

  hide() {
    this.el.hidden = true
    this.objectId = null
  }

  /** Redraw for the object's current mesh (after an edit came back). */
  set(object) {
    if (this.drag) return
    const islands = hasUvs(object.mesh) ? uvIslands(object.mesh) : []
    // Keep the selection on the island that holds the same first face.
    const keep = this.islands[this.selected]?.[0]
    this.mesh = object.mesh
    this.islands = islands
    this.selected = keep === undefined ? -1 : islands.findIndex((i) => i.includes(keep))
    this.name = object.name
    this.draw()
  }

  render(object) {
    const methods = [
      ['smart', 'Smart'],
      ['cube', 'Cube'],
      ['cylinder', 'Cylinder'],
      ['seams', 'Seams'],
    ]
    this.el.innerHTML = `<div class="uv-bar"><b>UV</b><span class="muted small" id="uv-name"></span><button class="uv-close" data-uv-close title="Close">×</button></div>
      <div class="uv-row"><span class="muted small">Unwrap</span>${methods.map(([m, l]) => `<button class="chip" data-unwrap="${m}">${l}</button>`).join('')}</div>
      <canvas class="uv-canvas" width="${SIZE * 2}" height="${SIZE * 2}"></canvas>
      <div class="uv-row"><span class="muted small" id="uv-hint"></span>
        <button class="chip" data-uv-op="rotate" title="Turn the island 90°">↻ 90°</button>
        <button class="chip" data-uv-op="shrink" title="Scale down">−</button>
        <button class="chip" data-uv-op="grow" title="Scale up">+</button>
        <button class="chip" data-uv-op="fill" title="Scale the island to fill the square">Fill</button></div>`
    this.canvas = this.el.querySelector('canvas')
    this.set(object)
  }

  /** UV → canvas pixels (v up). */
  px([u, v]) {
    return [u * SIZE * 2, (1 - v) * SIZE * 2]
  }

  draw() {
    const g = this.canvas?.getContext('2d')
    if (!g) return
    const s = SIZE * 2
    g.clearRect(0, 0, s, s)
    g.globalAlpha = 0.55
    g.drawImage(this.grid, 0, 0, s, s)
    g.globalAlpha = 1
    this.el.querySelector('#uv-name').textContent = this.name || ''
    const hint = this.el.querySelector('#uv-hint')
    for (const b of this.el.querySelectorAll('[data-uv-op]')) b.disabled = this.selected < 0
    if (!this.mesh || !hasUvs(this.mesh)) {
      hint.textContent = 'Not unwrapped yet'
      return
    }
    hint.textContent = this.selected >= 0 ? `Island ${this.selected + 1} of ${this.islands.length}` : `${this.islands.length} islands · click one`
    const d = this.drag?.moved ? this.drag.delta : [0, 0]
    this.islands.forEach((faces, i) => {
      const sel = i === this.selected
      g.fillStyle = sel ? 'rgba(255,122,61,0.35)' : 'rgba(20,22,26,0.28)'
      g.strokeStyle = sel ? '#ffb07f' : 'rgba(255,255,255,0.85)'
      g.lineWidth = sel ? 2 : 1
      for (const f of faces) {
        g.beginPath()
        this.mesh.uvs[f].forEach((uv, k) => {
          const p = this.px(sel ? [uv[0] + d[0], uv[1] + d[1]] : uv)
          if (k) g.lineTo(p[0], p[1])
          else g.moveTo(p[0], p[1])
        })
        g.closePath()
        g.fill()
        g.stroke()
      }
    })
  }

  /** The island under canvas-relative point (u, v), or -1. */
  islandAt(u, v) {
    const inside = (poly) => {
      let hit = false
      for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
        const [xi, yi] = poly[i]
        const [xj, yj] = poly[j]
        if (yi > v !== yj > v && u < ((xj - xi) * (v - yi)) / (yj - yi) + xi) hit = !hit
      }
      return hit
    }
    return this.islands.findIndex((faces) => faces.some((f) => inside(this.mesh.uvs[f])))
  }

  uvFromEvent(e) {
    const r = this.canvas.getBoundingClientRect()
    return [(e.clientX - r.left) / r.width, 1 - (e.clientY - r.top) / r.height]
  }

  down(e) {
    if (e.target !== this.canvas || !this.mesh || !hasUvs(this.mesh)) return
    e.preventDefault()
    const [u, v] = this.uvFromEvent(e)
    this.selected = this.islandAt(u, v)
    this.drag = this.selected >= 0 ? { start: [u, v], delta: [0, 0], moved: false } : null
    this.draw()
  }

  move(e) {
    if (!this.drag) return
    const [u, v] = this.uvFromEvent(e)
    this.drag.delta = [u - this.drag.start[0], v - this.drag.start[1]]
    this.drag.moved ||= Math.hypot(...this.drag.delta) > 0.004
    this.draw()
  }

  up() {
    const d = this.drag
    if (!d) return
    this.drag = null
    if (!d.moved) return this.draw()
    const r = (x) => Math.round(x * 1e5) / 1e5
    this.onTransform(this.islands[this.selected], { offset: d.delta.map(r) })
  }

  click(e) {
    const t = e.target
    if (t.closest('[data-uv-close]')) return this.onClose()
    const unwrap = t.closest('[data-unwrap]')
    if (unwrap) return this.onUnwrap(unwrap.dataset.unwrap)
    const op = t.closest('[data-uv-op]')?.dataset.uvOp
    if (!op || this.selected < 0) return
    const faces = this.islands[this.selected]
    if (op === 'rotate') return this.onTransform(faces, { rotate: Math.round((Math.PI / 2) * 1e6) / 1e6 })
    if (op === 'shrink') return this.onTransform(faces, { scale: 0.8 })
    if (op === 'grow') return this.onTransform(faces, { scale: 1.25 })
    if (op === 'fill') {
      // Scale up to fill the square (keeping its shape), then centre it.
      let [lo, hi] = [
        [Infinity, Infinity],
        [-Infinity, -Infinity],
      ]
      for (const f of faces)
        for (const [u, v] of this.mesh.uvs[f]) {
          lo = [Math.min(lo[0], u), Math.min(lo[1], v)]
          hi = [Math.max(hi[0], u), Math.max(hi[1], v)]
        }
      const scale = 0.98 / Math.max(hi[0] - lo[0], hi[1] - lo[1])
      const centre = [(lo[0] + hi[0]) / 2, (lo[1] + hi[1]) / 2]
      const r = (x) => Math.round(x * 1e5) / 1e5
      return this.onTransform(faces, { scale: r(scale), offset: [r(0.5 - centre[0]), r(0.5 - centre[1])] })
    }
  }
}
