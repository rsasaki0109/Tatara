// Node editor for node-based materials (src/nodes.rs). The graph is plain
// JSON in the material's texture; every edit here sends the whole graph back
// as one `material` command, so agents and people edit the same thing.

const PATTERNS = ['checker', 'stripes', 'tiles', 'brick', 'wood', 'marble']
const MATH_OPS = ['add', 'subtract', 'multiply', 'divide', 'power', 'minimum', 'maximum', 'greater_than', 'less_than', 'absolute', 'smoothstep']

// Per type: inputs [name, kind, default] and fields [name, options].
export const NODE_TYPES = {
  noise: { label: 'Noise', inputs: [['scale', 'number', 4], ['detail', 'number', 4], ['warp', 'number', 0]] },
  voronoi: { label: 'Voronoi', inputs: [['scale', 'number', 4], ['warp', 'number', 0]], fields: [['output', ['distance', 'cells', 'edges']]] },
  pattern: { label: 'Pattern', inputs: [['scale', 'number', 1], ['warp', 'number', 0]], fields: [['pattern', PATTERNS]] },
  gradient: { label: 'Gradient', inputs: [], fields: [['direction', ['u', 'v', 'radial']]] },
  image: { label: 'Image', inputs: [['scale', 'number', 1]], fields: [['image', 'images']] },
  mix: { label: 'Mix', inputs: [['a', 'color', '#000000'], ['b', 'color', '#ffffff'], ['factor', 'number', 0.5]] },
  math: { label: 'Math', inputs: [['a', 'number', 0], ['b', 'number', 0]], fields: [['op', MATH_OPS]] },
  ramp: { label: 'Color ramp', inputs: [['factor', 'number', 0.5]], ramp: true },
}
const OUTPUTS = [
  ['color', 'color', '#9aa0a6'],
  ['roughness', 'number', 0.5],
  ['metalness', 'number', 0],
  ['height', 'number', 0.5],
]

const link = (node) => ({ node })
const at = (x, y) => [x, y]

/** Starting graph for a new node material: noise through a colour ramp. */
export function starterGraph(color = '#9aa0a6') {
  return {
    nodes: [
      { id: 'noise', type: 'noise', scale: 4, detail: 4, warp: 0, at: at(10, 20) },
      { id: 'ramp', type: 'ramp', factor: link('noise'), stops: [{ at: 0.2, color: '#2e3036' }, { at: 0.8, color }], at: at(195, 20) },
    ],
    output: { color: link('ramp'), height: link('noise'), at: at(380, 30) },
  }
}

// Ready-made graphs, also used by the demo.
export const NODE_PRESETS = {
  rust: {
    label: 'Rusty metal',
    graph: {
      nodes: [
        { id: 'noise', type: 'noise', scale: 3, detail: 5, at: at(10, 10) },
        { id: 'rust', type: 'math', op: 'smoothstep', a: link('noise'), b: 0.06, at: at(10, 135) },
        { id: 'paint', type: 'ramp', factor: link('noise'), stops: [{ at: 0.35, color: '#a9b0b8' }, { at: 0.52, color: '#8d4520' }, { at: 0.9, color: '#4f2410' }], at: at(195, 10) },
        { id: 'rough', type: 'mix', a: 0.22, b: 0.9, factor: link('rust'), at: at(380, 10) },
        { id: 'metal', type: 'math', op: 'subtract', a: 1, b: link('rust'), at: at(380, 140) },
      ],
      output: { color: link('paint'), roughness: link('rough'), metalness: link('metal'), height: link('rust'), at: at(565, 30) },
    },
  },
  stone: {
    label: 'Stone wall',
    graph: {
      nodes: [
        { id: 'cells', type: 'voronoi', scale: 5, output: 'cells', at: at(10, 10) },
        { id: 'edges', type: 'voronoi', scale: 5, output: 'edges', at: at(10, 135) },
        { id: 'tone', type: 'ramp', factor: link('cells'), stops: [{ at: 0, color: '#6f6a62' }, { at: 0.5, color: '#9c968b' }, { at: 1, color: '#7d776d' }], at: at(195, 10) },
        { id: 'joint', type: 'math', op: 'smoothstep', a: link('edges'), b: 0.35, at: at(380, 140) },
        { id: 'color', type: 'mix', a: '#2f2c29', b: link('tone'), factor: link('joint'), at: at(380, 10) },
      ],
      output: { color: link('color'), roughness: 0.9, height: link('joint'), at: at(565, 30) },
    },
  },
  // Voronoi cells become chips (cells past 0.55 on the ramp blend into the
  // ground), separated by their edges.
  terrazzo: {
    label: 'Terrazzo',
    graph: {
      nodes: [
        { id: 'chips', type: 'voronoi', scale: 12, output: 'cells', at: at(10, 10) },
        { id: 'edges', type: 'voronoi', scale: 12, output: 'edges', at: at(10, 135) },
        { id: 'palette', type: 'ramp', factor: link('chips'), constant: true, stops: [{ at: 0, color: '#c9563c' }, { at: 0.14, color: '#2f5d62' }, { at: 0.28, color: '#e0a83a' }, { at: 0.42, color: '#3b3a36' }, { at: 0.55, color: '#ebe6dc' }], at: at(195, 10) },
        { id: 'mask', type: 'math', op: 'greater_than', a: link('edges'), b: 0.22, at: at(380, 140) },
        { id: 'color', type: 'mix', a: '#ebe6dc', b: link('palette'), factor: link('mask'), at: at(380, 10) },
      ],
      output: { color: link('color'), roughness: 0.3, at: at(565, 30) },
    },
  },
}

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c])
const isLink = (v) => v && typeof v === 'object' && 'node' in v
const clone = (g) => JSON.parse(JSON.stringify(g))

export class NodeEditor {
  /**
   * `el` hosts the editor; `onChange(graph)` commits an edited graph;
   * `images()` lists the scene's image names for image nodes.
   */
  constructor(el, { onChange, images, onClose }) {
    this.el = el
    this.onChange = onChange
    this.images = images
    this.onClose = onClose
    this.graph = null
    this.drag = null
    el.addEventListener('pointerdown', (e) => this.down(e))
    window.addEventListener('pointermove', (e) => this.move(e))
    window.addEventListener('pointerup', (e) => this.up(e))
    el.addEventListener('change', (e) => this.change(e))
    el.addEventListener('click', (e) => this.click(e))
  }

  get open() {
    return !this.el.hidden
  }

  show(graph, title) {
    this.el.hidden = false
    this.title = title
    this.set(graph)
  }

  hide() {
    this.el.hidden = true
    this.graph = null
  }

  /** Show `graph` (after an edit came back from the engine). */
  set(graph) {
    if (this.drag) return
    this.graph = clone(graph)
    this.render()
  }

  commit() {
    this.onChange(clone(this.graph))
  }

  node(id) {
    return this.graph.nodes.find((n) => n.id === id)
  }

  render() {
    const g = this.graph
    const add = Object.entries(NODE_TYPES)
      .map(([t, s]) => `<option value="${t}">${s.label}</option>`)
      .join('')
    const presets = Object.entries(NODE_PRESETS)
      .map(([k, p]) => `<option value="${k}">${p.label}</option>`)
      .join('')
    const nodes = g.nodes.map((n) => this.nodeHtml(n)).join('')
    const out = g.output || {}
    const o = out.at || [380, 30]
    const outRows = OUTPUTS.map(([name, kind]) => this.inputRow('output', name, kind, out[name], true)).join('')
    this.el.innerHTML = `<div class="ne-bar"><b>Nodes</b><span class="muted small">${esc(this.title || '')}</span>
        <select id="ne-add"><option value="">+ Add node</option>${add}</select>
        <select id="ne-preset"><option value="">Presets…</option>${presets}</select>
        <button class="ne-close" data-ne-close title="Close">×</button></div>
      <div class="ne-canvas"><svg class="ne-wires"></svg>${nodes}
        <div class="ne-node ne-output" data-node="output" style="left:${o[0]}px;top:${o[1]}px">
          <div class="ne-head" data-move="output"><span>Material output</span></div>${outRows}</div>
      </div>`
    this.wires()
  }

  nodeHtml(n) {
    const spec = NODE_TYPES[n.type]
    const [x, y] = n.at || [20, 20]
    const fields = (spec.fields || [])
      .map(([name, options]) => {
        const list = options === 'images' ? this.images() : options
        const opts = list.map((v) => `<option ${n[name] === v ? 'selected' : ''}>${esc(v)}</option>`).join('')
        return `<div class="ne-row"><label>${name}</label><select data-field="${n.id}:${name}">${opts}</select></div>`
      })
      .join('')
    const inputs = spec.inputs.map(([name, kind, d]) => this.inputRow(n.id, name, kind, n[name] ?? d)).join('')
    const stops = spec.ramp
      ? `<div class="ne-ramp" style="background:linear-gradient(90deg,${[...n.stops]
          .sort((a, b) => a.at - b.at)
          .map((s) => `${s.color} ${s.at * 100}%`)
          .join(',')})"></div>` +
        n.stops
          .map(
            (s, i) =>
              `<div class="ne-row ne-stop"><input type="number" min="0" max="1" step="0.05" data-stop="${n.id}:${i}:at" value="${s.at}"><input type="color" data-stop="${n.id}:${i}:color" value="${s.color}"><button data-stop-del="${n.id}:${i}" title="Remove stop">−</button></div>`,
          )
          .join('') +
        `<div class="ne-row"><button data-stop-add="${n.id}">+ stop</button><label class="ne-check"><input type="checkbox" data-constant="${n.id}" ${n.constant ? 'checked' : ''}> steps</label></div>`
      : ''
    return `<div class="ne-node" data-node="${n.id}" style="left:${x}px;top:${y}px">
      <div class="ne-head" data-move="${n.id}"><span>${spec.label}</span><button data-del="${n.id}" title="Delete">×</button><i class="ne-sock ne-out" data-out="${n.id}"></i></div>
      ${fields}${inputs}${stops}</div>`
  }

  inputRow(id, name, kind, value, output = false) {
    const linked = isLink(value)
    let control = ''
    if (!linked) {
      if (output && value === undefined) control = '<span class="muted small">material</span>'
      else if (kind === 'color' || typeof value === 'string')
        control = `<input type="color" data-input="${id}:${name}" value="${typeof value === 'string' ? value : '#808080'}">`
      else control = `<input type="number" step="0.05" data-input="${id}:${name}" value="${value ?? 0}">`
    }
    const clear = output && value !== undefined && !linked ? `<button data-unset="${name}" title="Use the material's own value">×</button>` : ''
    return `<div class="ne-row" data-row="${id}:${name}"><i class="ne-sock ne-in ${linked ? 'on' : ''}" data-in="${id}:${name}"></i><label>${name}</label>${control}${clear}</div>`
  }

  /** Socket centre relative to the canvas. */
  socket(sel) {
    const s = this.el.querySelector(sel)
    if (!s) return null
    const c = this.el.querySelector('.ne-canvas').getBoundingClientRect()
    const r = s.getBoundingClientRect()
    return [r.left + r.width / 2 - c.left, r.top + r.height / 2 - c.top]
  }

  wires(extra = null) {
    const svg = this.el.querySelector('.ne-wires')
    if (!svg) return
    const curve = ([x1, y1], [x2, y2], cls = '') => {
      const dx = Math.max(40, Math.abs(x2 - x1) / 2)
      return `<path class="${cls}" d="M${x1},${y1} C${x1 + dx},${y1} ${x2 - dx},${y2} ${x2},${y2}"/>`
    }
    const paths = []
    const each = (id, name, value) => {
      if (!isLink(value)) return
      const a = this.socket(`[data-out="${value.node}"]`)
      const b = this.socket(`[data-in="${id}:${name}"]`)
      if (a && b) paths.push(curve(a, b))
    }
    for (const n of this.graph.nodes) for (const [name] of NODE_TYPES[n.type].inputs) each(n.id, name, n[name])
    for (const [name] of OUTPUTS) each('output', name, this.graph.output?.[name])
    if (extra) paths.push(curve(extra[0], extra[1], 'ne-live'))
    svg.innerHTML = paths.join('')
  }

  down(e) {
    const out = e.target.closest('[data-out]')
    const head = e.target.closest('[data-move]')
    const c = this.el.querySelector('.ne-canvas')?.getBoundingClientRect()
    if (out) {
      e.preventDefault()
      this.drag = { kind: 'wire', from: out.dataset.out, start: this.socket(`[data-out="${out.dataset.out}"]`), c }
    } else if (head && !e.target.closest('button')) {
      e.preventDefault()
      const id = head.dataset.move
      const box = this.el.querySelector(`[data-node="${id}"]`)
      this.drag = { kind: 'move', id, box, x: e.clientX - box.offsetLeft, y: e.clientY - box.offsetTop }
    }
  }

  move(e) {
    const d = this.drag
    if (!d) return
    if (d.kind === 'move') {
      const x = Math.max(0, e.clientX - d.x)
      const y = Math.max(0, e.clientY - d.y)
      d.box.style.left = `${x}px`
      d.box.style.top = `${y}px`
      d.at = [Math.round(x), Math.round(y)]
      this.wires()
    } else {
      this.wires([d.start, [e.clientX - d.c.left, e.clientY - d.c.top]])
    }
  }

  up(e) {
    const d = this.drag
    if (!d) return
    this.drag = null
    if (d.kind === 'move') {
      if (!d.at) return
      if (d.id === 'output') this.graph.output = { ...this.graph.output, at: d.at }
      else this.node(d.id).at = d.at
      return this.commit()
    }
    const target = document.elementFromPoint(e.clientX, e.clientY)?.closest('[data-in]')
    if (!target) return this.wires()
    const [id, name] = target.dataset.in.split(':')
    if (id === d.from) return this.wires()
    this.setInput(id, name, link(d.from))
    if (this.cycles()) {
      this.toast?.('That link would make a loop')
      return this.render()
    }
    this.render()
    this.commit()
  }

  /** The first grid cell no node covers, so new nodes land in view. */
  freeSpot() {
    const boxes = [...this.graph.nodes.map((n) => n.at || [10, 10]), this.graph.output?.at || [380, 30]]
    for (const y of [10, 150])
      for (const x of [10, 195, 380, 565]) if (!boxes.some(([bx, by]) => Math.abs(bx - x) < 170 && Math.abs(by - y) < 125)) return [x, y]
    return [20 + this.graph.nodes.length * 12, 40 + this.graph.nodes.length * 12]
  }

  /** Whether the graph has a loop (the engine would reject it). */
  cycles() {
    const state = {}
    const deps = (n) => NODE_TYPES[n.type].inputs.map(([k]) => n[k]).filter(isLink).map((l) => l.node)
    const visit = (id) => {
      if (state[id] === 1) return true
      if (state[id] === 2) return false
      state[id] = 1
      const n = this.node(id)
      if (n && deps(n).some(visit)) return true
      state[id] = 2
      return false
    }
    return this.graph.nodes.some((n) => visit(n.id))
  }

  setInput(id, name, value) {
    if (id === 'output') {
      const out = { ...this.graph.output }
      if (value === undefined) delete out[name]
      else out[name] = value
      this.graph.output = out
    } else this.node(id)[name] = value
  }

  change(e) {
    const t = e.target
    if (t.id === 'ne-add' && t.value) {
      const type = t.value
      const spec = NODE_TYPES[type]
      let k = 1
      while (this.node(`${type}${k}`)) k++
      const n = { id: `${type}${k}`, type, at: this.freeSpot() }
      for (const [name, , d] of spec.inputs) n[name] = d
      for (const [name, options] of spec.fields || []) n[name] = (options === 'images' ? this.images() : options)[0] ?? ''
      if (spec.ramp) n.stops = [{ at: 0, color: '#000000' }, { at: 1, color: '#ffffff' }]
      if (type === 'image' && !n.image) return this.render()
      this.graph.nodes.push(n)
      this.render()
      return this.commit()
    }
    if (t.id === 'ne-preset' && t.value) {
      this.graph = clone(NODE_PRESETS[t.value].graph)
      this.render()
      return this.commit()
    }
    if (t.dataset.input) {
      const [id, name] = t.dataset.input.split(':')
      this.setInput(id, name, t.type === 'color' ? t.value : Number(t.value))
    } else if (t.dataset.field) {
      const [id, name] = t.dataset.field.split(':')
      this.node(id)[name] = t.value
    } else if (t.dataset.stop) {
      const [id, i, key] = t.dataset.stop.split(':')
      const stop = this.node(id).stops[Number(i)]
      stop[key] = key === 'at' ? Math.min(1, Math.max(0, Number(t.value))) : t.value
    } else if (t.dataset.constant) {
      this.node(t.dataset.constant).constant = t.checked
    } else return
    this.render()
    this.commit()
  }

  click(e) {
    const t = e.target
    if (t.closest('[data-ne-close]')) return this.onClose()
    const del = t.closest('[data-del]')
    if (del) {
      const id = del.dataset.del
      this.graph.nodes = this.graph.nodes.filter((n) => n.id !== id)
      // Inputs that read the deleted node fall back to their defaults.
      for (const n of this.graph.nodes)
        for (const [name, , d] of NODE_TYPES[n.type].inputs) if (isLink(n[name]) && n[name].node === id) n[name] = d
      for (const [name] of OUTPUTS) if (isLink(this.graph.output?.[name]) && this.graph.output[name].node === id) this.setInput('output', name, undefined)
      this.render()
      return this.commit()
    }
    const sock = t.closest('.ne-in.on')
    if (sock) {
      // Click a connected input to disconnect it.
      const [id, name] = sock.dataset.in.split(':')
      const d = id === 'output' ? undefined : NODE_TYPES[this.node(id).type].inputs.find(([k]) => k === name)[2]
      this.setInput(id, name, d)
      this.render()
      return this.commit()
    }
    const unset = t.closest('[data-unset]')
    if (unset) {
      this.setInput('output', unset.dataset.unset, undefined)
      this.render()
      return this.commit()
    }
    const addStop = t.closest('[data-stop-add]')
    if (addStop) {
      const n = this.node(addStop.dataset.stopAdd)
      if (n.stops.length >= 16) return
      n.stops.push({ at: 0.5, color: '#808080' })
      this.render()
      return this.commit()
    }
    const delStop = t.closest('[data-stop-del]')
    if (delStop) {
      const [id, i] = delStop.dataset.stopDel.split(':')
      const n = this.node(id)
      if (n.stops.length <= 1) return
      n.stops.splice(Number(i), 1)
      this.render()
      return this.commit()
    }
  }
}
