// The Render panel: a finished, path-traced image of the current view at a
// chosen size, refined pass by pass by the core (`/api/pathtrace`) until it
// reaches the sample count, with optional depth of field focused on the
// selection. Save it as a PNG, or ask the core for an animated PNG.

const SIZES = [
  [1280, 720],
  [1920, 1080],
  [1080, 1080],
  [960, 540],
]
const SAMPLES = [32, 128, 512]

/** The viewport's backdrop (its CSS radial gradient), drawn on a 2D context. */
export function drawBackdrop(g, w, h) {
  g.save()
  // An ellipse 120% x 90% around (50%, 38%): scale a circle into it.
  g.translate(w * 0.5, h * 0.38)
  g.scale(1.2 * w, 0.9 * h)
  const grad = g.createRadialGradient(0, 0, 0, 0, 0, 1)
  grad.addColorStop(0, '#4a4c53')
  grad.addColorStop(0.48, '#303238')
  grad.addColorStop(1, '#1b1c20')
  g.fillStyle = grad
  g.fillRect(-1, -1, 2, 2)
  g.restore()
}

export class FinalRender {
  /**
   * `camera()` gives the view to render ({eye, target, fov}); `focusPoint()`
   * the world point to focus on (the selection) or null; `frame()` the
   * current frame and `range()` the animation's; `clock` tracks requests.
   */
  constructor(el, { camera, focusPoint, frame, range, clock, onChange = () => {} }) {
    Object.assign(this, { el, camera, focusPoint, frame, range, clock, onChange })
    this.size = SIZES[0]
    this.target = SAMPLES[1]
    this.dof = false
    this.blur = 0.05
    this.transparent = false
    this.samples = 0
    this.job = 0
    this.running = false
    el.addEventListener('click', (e) => this.click(e))
    el.addEventListener('change', (e) => this.change(e))
    el.addEventListener('input', (e) => {
      if (e.target.id === 'fr-blur') e.target.nextElementSibling.textContent = e.target.value
    })
  }

  get open() {
    return !this.el.hidden
  }

  show() {
    this.el.hidden = false
    this.draw()
  }

  hide() {
    this.stop()
    this.el.hidden = true
  }

  stop() {
    this.job++
    this.running = false
    this.onChange()
  }

  draw() {
    const [w, h] = this.size
    const opt = (v, label, on) => `<option value="${v}" ${on ? 'selected' : ''}>${label}</option>`
    this.el.innerHTML = `<div class="fr-bar"><b>Render</b><span class="muted small" id="fr-status"></span><button class="fr-close" data-fr="close" title="Close">×</button></div>
      <div class="fr-row">
        <label class="inline">Size <select id="fr-size">${SIZES.map(([a, b], i) => opt(i, `${a}×${b}`, a === w && b === h)).join('')}</select></label>
        <label class="inline">Samples <select id="fr-samples">${SAMPLES.map((n) => opt(n, n, n === this.target)).join('')}</select></label>
      </div>
      <div class="fr-row">
        <label class="inline"><input type="checkbox" id="fr-dof" ${this.dof ? 'checked' : ''}> Depth of field</label>
        <label class="slider fr-blur"><span>Blur</span><input type="range" min="0.01" max="0.2" step="0.01" id="fr-blur" value="${this.blur}" ${this.dof ? '' : 'disabled'}><b>${this.blur}</b></label>
      </div>
      <div class="fr-row">
        <label class="inline">Background <select id="fr-bg">${opt('studio', 'Studio', !this.transparent)}${opt('transparent', 'Transparent', this.transparent)}</select></label>
      </div>
      <canvas class="fr-canvas" width="${w}" height="${h}"></canvas>
      <div class="fr-row end">
        <button class="small-btn" data-fr="animation" title="An animated PNG of the timeline (half size, at most 16 samples)">Animation</button>
        <button class="small-btn" data-fr="save" ${this.samples ? '' : 'disabled'}>Save PNG</button>
        <button class="small-btn primary" data-fr="render">${this.running ? 'Stop' : 'Render'}</button>
      </div>`
    this.canvas = this.el.querySelector('canvas')
    this.status()
  }

  status(text) {
    const el = this.el.querySelector('#fr-status')
    if (el) el.textContent = text ?? (this.samples ? `${this.samples} / ${this.target} samples` : '')
    const save = this.el.querySelector('[data-fr=save]')
    if (save) save.disabled = !this.samples
    const go = this.el.querySelector('[data-fr=render]')
    if (go) go.textContent = this.running ? 'Stop' : 'Render'
  }

  change(e) {
    const t = e.target
    if (t.id === 'fr-size') this.size = SIZES[Number(t.value)]
    if (t.id === 'fr-samples') this.target = Number(t.value)
    if (t.id === 'fr-dof') this.dof = t.checked
    if (t.id === 'fr-blur') this.blur = Number(t.value)
    if (t.id === 'fr-bg') this.transparent = t.value === 'transparent'
    this.stop()
    this.samples = 0
    this.draw()
  }

  click(e) {
    const action = e.target.closest('[data-fr]')?.dataset.fr
    if (action === 'close') return this.hide()
    if (action === 'render') return this.running ? this.stop() : this.render()
    if (action === 'save') return this.save()
    if (action === 'animation') return this.animation()
  }

  /** The camera and lens as query parameters. */
  params(w, h) {
    const c = this.camera()
    const r = (v) => v.toFixed(5)
    const q = {
      w: String(w),
      h: String(h),
      eye: c.eye.map(r).join(','),
      target: c.target.map(r).join(','),
      fov: c.fov.toFixed(3),
      frame: String(this.frame()),
    }
    if (this.dof) {
      const p = this.focusPoint()
      const d = p ? Math.hypot(p[0] - c.eye[0], p[1] - c.eye[1], p[2] - c.eye[2]) : 0
      Object.assign(q, { aperture: String(this.blur), focus: d.toFixed(4) })
    }
    return q
  }

  /** Refine the image pass by pass until it holds the target samples. */
  async render() {
    const job = ++this.job
    this.running = true
    this.samples = 0
    this.onChange()
    this.status()
    const [w, h] = this.size
    const params = this.params(w, h)
    while (job === this.job && this.samples < this.target) {
      const samples = Math.min(this.samples < 4 ? 2 : 8, this.target - this.samples)
      const res = await this.clock.track(fetch(`/api/pathtrace?${new URLSearchParams({ ...params, samples: String(samples) })}`))
      if (!res.ok) {
        this.status('Render failed')
        break
      }
      const buf = await this.clock.track(res.arrayBuffer())
      if (job !== this.job) return
      this.samples = new DataView(buf).getUint32(0, true)
      this.paint(new ImageData(new Uint8ClampedArray(buf, 4, w * h * 4), w, h))
      this.status()
    }
    if (job === this.job) {
      this.running = false
      this.onChange()
      this.status()
    }
  }

  /** Show a pass over the studio backdrop, or alone when transparent. */
  paint(image) {
    this.pass ??= document.createElement('canvas')
    this.pass.width = image.width
    this.pass.height = image.height
    this.pass.getContext('2d').putImageData(image, 0, 0)
    const g = this.canvas.getContext('2d')
    g.clearRect(0, 0, image.width, image.height)
    if (!this.transparent) drawBackdrop(g, image.width, image.height)
    g.drawImage(this.pass, 0, 0)
  }

  async save() {
    const blob = await new Promise((resolve) => this.canvas.toBlob(resolve, 'image/png'))
    this.download(blob, 'render.png')
  }

  async animation() {
    const [w, h] = this.size.map((v) => Math.max(8, Math.round(v / 2)))
    const { start, end } = this.range()
    const q = { ...this.params(w, h), samples: String(Math.min(16, this.target)), frames: `${start}-${Math.min(end, start + 239)}` }
    delete q.frame
    if (this.transparent) q.background = 'transparent'
    this.status('Rendering the animation…')
    const res = await this.clock.track(fetch(`/api/render/image?${new URLSearchParams(q)}`))
    if (!res.ok) return this.status(`Animation failed: ${(await res.text()).slice(0, 80)}`)
    this.download(await res.blob(), 'animation.png')
    this.status()
  }

  download(blob, name) {
    const a = document.createElement('a')
    a.href = URL.createObjectURL(blob)
    a.download = name
    a.click()
    setTimeout(() => URL.revokeObjectURL(a.href), 1000)
  }
}
