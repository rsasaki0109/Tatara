// Rendered preview: the viewport camera's view, path traced by the Rust
// core (`/api/pathtrace`) and refined pass by pass, over the live scene.
//
// The server keeps adding samples to one image while the camera, scene and
// frame stay put, and answers each pass with the denoised result; any
// change starts over. While the camera moves, the raster view shows.

const MAX_SAMPLES = 512
/// Settle time after the camera stops before tracing starts again.
const SETTLE_MS = 120

export class PathPreview {
  /**
   * `viewport` supplies the camera; `clock` tracks requests so recordings
   * wait for them; `budget` caps the traced pixels (smaller in the browser
   * build, where tracing shares the page's thread).
   */
  constructor(viewport, clock, { budget = 360000, onUpdate = () => {} } = {}) {
    this.viewport = viewport
    this.clock = clock
    this.budget = budget
    this.onUpdate = onUpdate
    this.active = false
    this.samples = 0
    this.key = null
    this.since = 0
    this.busy = false
    this.revision = null
    this.canvas = document.createElement('canvas')
    this.canvas.className = 'pt-canvas'
    this.canvas.hidden = true
    viewport.renderer.domElement.after(this.canvas)
  }

  setActive(on) {
    this.active = on
    this.key = null
    this.samples = 0
    this.show(false)
    this.onUpdate()
  }

  /** Show the traced image in place of the raster view, or not. */
  show(on) {
    this.canvas.hidden = !on
    // Transparent, not hidden: the raster canvas still takes the orbit drags.
    this.viewport.renderer.domElement.style.opacity = on ? '0' : ''
  }

  /** The scene changed: refine from scratch. */
  setRevision(revision) {
    if (revision === this.revision) return
    this.revision = revision
    this.key = null
  }

  /** Pixel size to trace at: the viewport's, scaled to the budget. */
  size() {
    const el = this.viewport.renderer.domElement
    const w = el.clientWidth || 1
    const h = el.clientHeight || 1
    const s = Math.min(1, Math.sqrt(this.budget / (w * h)))
    return [Math.max(8, Math.round(w * s)), Math.max(8, Math.round(h * s))]
  }

  params() {
    const cam = this.viewport.camera
    const [w, h] = this.size()
    const r = (v) => v.toFixed(5)
    const eye = cam.position
    const target = this.viewport.controls.target
    if (this.viewport.sceneCameraId != null) return { w: String(w), h: String(h), camera: String(this.viewport.sceneCameraId), frame: String(this.viewport.currentFrame ?? 1) }
    return {
      w: String(w),
      h: String(h),
      eye: [eye.x, eye.y, eye.z].map(r).join(','),
      target: [target.x, target.y, target.z].map(r).join(','),
      ...(cam.isOrthographicCamera ? {ortho_height:String(this.viewport.effectiveOrthoHeight())} : {fov:cam.fov.toFixed(3)}),
      up:cam.up.toArray().map(r).join(','),
      frame: String(this.viewport.currentFrame ?? 1),
    }
  }

  /** Called every frame: start the next pass once things are still. */
  tick() {
    // A final render uses the core's tracer meanwhile.
    if (!this.active || this.busy || this.paused) return
    const params = this.params()
    const key = JSON.stringify([params, this.revision])
    const now = this.clock.now()
    if (key !== this.key) {
      this.key = key
      this.since = now
      this.samples = 0
      this.show(false)
      this.onUpdate()
      return
    }
    if (now - this.since < SETTLE_MS || this.samples >= MAX_SAMPLES) return
    // Small first passes answer quickly; later ones amortize the overhead.
    const samples = this.samples < 2 ? 1 : this.samples < 8 ? 2 : 4
    this.busy = true
    const query = new URLSearchParams({ ...params, samples: String(samples) })
    this.clock
      .track(fetch(`/api/pathtrace?${query}`).then((r) => (r.ok ? r.arrayBuffer() : Promise.reject(new Error(r.statusText)))))
      .then((buf) => {
        if (key !== this.key || !this.active) return
        this.draw(buf, Number(params.w), Number(params.h))
      })
      .catch(() => {
        // Leave the raster view up; the next change retries.
        this.samples = MAX_SAMPLES
      })
      .finally(() => {
        this.busy = false
      })
  }

  draw(buf, w, h) {
    const view = new DataView(buf)
    this.samples = view.getUint32(0, true)
    const pixels = new Uint8ClampedArray(buf, 4, w * h * 4)
    if (this.canvas.width !== w || this.canvas.height !== h) {
      this.canvas.width = w
      this.canvas.height = h
    }
    this.canvas.getContext('2d').putImageData(new ImageData(pixels, w, h), 0, 0)
    this.show(true)
    this.onUpdate()
  }
}
