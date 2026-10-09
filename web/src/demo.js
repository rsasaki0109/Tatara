// Scripted tool workflows. Every step drives the real editor: toolbar clicks,
// face picks and command batches all go through the same Rust command API.
// Timing uses the shared clock, so the capture recorder can step it frame by
// frame and produce identical recordings every run.

import * as THREE from 'three'
import { ease } from './clock.js'

const $ = (id) => document.getElementById(id)

function escapeHtml(s) {
  return s.replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' })[c])
}

export class DemoRunner {
  constructor(app) {
    this.app = app
    this.running = false
    this.pos = { x: 0, y: 0 }
    this.captionN = 0
  }

  get clock() {
    return this.app.clock
  }

  sleep(ms) {
    return this.clock.sleep(ms)
  }

  async idle() {
    while (this.app.inflight > 0) await this.sleep(16)
  }

  resolveId(ref) {
    if (typeof ref === 'number') return ref
    const o = this.app.scene.objects.find((x) => x.name === ref)
    if (!o) throw new Error(`demo: no object named ${ref}`)
    return o.id
  }

  async play(scenario) {
    const { app } = this
    this.running = true
    this.captionN = 0
    try {
      if (app.capture) await app.api.reset()
      else if (app.scene.objects.length) await app.api.commands([{ op: 'clear' }])
      app.select(null)
      app.activity.length = 0
      app.actions.wireframe(false)
      app.actions.rendered(false)
      app.showTab(scenario.tab || 'properties')
      if (scenario.setup) await app.api.commands(scenario.setup, undefined, 'setup')
      await app.refresh(false)
      const vp = app.viewport
      vp.spinRate = 0
      if (app.playing) app.togglePlay(false)
      app.setFrame(app.scene.animation?.start ?? 1)
      vp.showGizmo = scenario.gizmo ?? true
      if (scenario.camera) vp.setOrbit(scenario.camera)
      const rect = document.body.getBoundingClientRect()
      this.pos = { x: rect.width * 0.62, y: rect.height * 0.7 }
      this.placeCursor(0)
      for (const step of scenario.steps) await this.step(step)
    } finally {
      this.running = false
      this.app.replaying = false
      if (!app.capture) {
        this.fade($('caption'), 0, 300)
        this.fade($('terminal'), 0, 300)
        this.fade($('cursor'), 0, 300)
        app.viewport.spinRate = 0
        app.viewport.showGizmo = true
        app.viewport.setSelection(app.selected, app.face)
        app.render()
      }
    }
  }

  async step(s) {
    const { app } = this
    const vp = app.viewport
    if (s.caption !== undefined) return this.caption(s.caption, s.hint)
    if (s.wait) return this.sleep(s.wait)
    if (s.run) {
      const r = await app.run(s.run, s.source || 'UI')
      if (s.select === 'created') app.select(r.created[0])
      return s.after ? this.sleep(s.after) : undefined
    }
    if (s.click) return this.click(s.click, s)
    if (s.pick !== undefined) return this.pick(s)
    if (s.camera) {
      const p = vp.orbitTo(s.camera, s.ms ?? 1200)
      return s.async ? undefined : p
    }
    if (s.frame) {
      const p = vp.frameAll(s.frame.ms ?? 1000, s.frame)
      return s.async ? undefined : p
    }
    if (s.spin !== undefined) {
      vp.spinRate = s.spin
      return
    }
    if (s.tab) return app.showTab(s.tab)
    if (s.select !== undefined && Object.keys(s).length === 1) return app.select(s.select === null ? null : this.resolveId(s.select))
    if (s.type) {
      await this.type(s.into, s.type, s.cps)
      await this.idle()
      return s.after ? this.sleep(s.after) : undefined
    }
    if (s.slide) return this.slide(s)
    if (s.drag) return this.dragNumber(s)
    if (s.stroke) return this.stroke(s)
    if (s.choose) return this.choose(s)
    if (s.upload) return this.upload(s)
    if (s.wire) return this.wire(s)
    if (s.uv !== undefined) return this.uv(s)
    if (s.samples) return this.samples(s)
    if (s.reach) return this.reach(s)
    if (s.hover) {
      // Point at an element: the cursor moves there and it sees the mouse.
      const el = document.querySelector(s.hover)
      if (!el) throw new Error(`demo: nothing matches ${s.hover}`)
      const r = el.getBoundingClientRect()
      await this.moveTo(r.left + r.width * 0.5, r.top + r.height * 0.5)
      el.dispatchEvent(new MouseEvent('mouseover', { bubbles: true }))
      await this.idle()
      return s.after ? this.sleep(s.after) : undefined
    }
    if (s.leave) {
      document.querySelector(s.leave)?.dispatchEvent(new MouseEvent('mouseleave'))
      await this.idle()
      return s.after ? this.sleep(s.after) : undefined
    }
    if (s.reveal) {
      const el = document.querySelector(s.reveal)
      if (!el) throw new Error(`demo: nothing matches ${s.reveal}`)
      el.scrollIntoView({ block: 'end' })
      return s.after ? this.sleep(s.after) : undefined
    }
    if (s.scrub !== undefined) return this.scrub(s)
    if (s.chat) return this.chat(s)
    if (s.mcp) return this.mcp(s)
    if (s.terminal !== undefined) return this.fade($('terminal'), s.terminal ? 1 : 0, 300)
    if (s.cursor !== undefined) return this.fade($('cursor'), s.cursor ? 1 : 0, 250)
    throw new Error(`demo: unknown step ${JSON.stringify(s)}`)
  }

  fade(el, to, ms) {
    const from = Number(getComputedStyle(el).opacity)
    el.style.display = 'flex'
    return this.app.animator.add(`fade:${el.id}`, ms, (t) => {
      el.style.opacity = String(from + (to - from) * t)
      if (t === 1 && to === 0) el.style.display = 'none'
    })
  }

  async caption(text, hint) {
    const el = $('caption')
    if (!text) return this.fade(el, 0, 250)
    if (Number(getComputedStyle(el).opacity) > 0.01 && el.style.display !== 'none') await this.fade(el, 0, 160)
    this.captionN++
    $('caption-step').textContent = String(this.captionN)
    $('caption-text').textContent = text
    $('caption-hint').textContent = hint || ''
    $('caption-hint').style.display = hint ? '' : 'none'
    await this.fade(el, 1, 220)
  }

  // -- cursor ---------------------------------------------------------------

  placeCursor(opacity) {
    const c = $('cursor')
    c.style.transform = `translate(${this.pos.x}px, ${this.pos.y}px)`
    if (opacity !== undefined) {
      c.style.opacity = String(opacity)
      c.style.display = 'flex'
    }
  }

  async moveTo(x, y, ms = 400) {
    const c = $('cursor')
    if (Number(c.style.opacity || 0) < 0.99) this.fade(c, 1, 200)
    const from = { ...this.pos }
    const dist = Math.hypot(x - from.x, y - from.y)
    await this.app.animator.add(
      'cursor',
      Math.min(ms, 180 + dist * 0.6),
      (t) => {
        // A slight arc reads as a hand movement rather than a robot.
        const arc = Math.sin(Math.PI * t) * Math.min(40, dist * 0.08)
        this.pos = { x: from.x + (x - from.x) * t, y: from.y + (y - from.y) * t - arc }
        this.placeCursor()
      },
      ease.inOut,
    )
  }

  async press() {
    const ripple = document.querySelector('#cursor .ripple')
    await this.app.animator.add(
      'ripple',
      260,
      (t) => {
        ripple.style.opacity = String(0.9 * (1 - t))
        ripple.style.transform = `translate(-50%, -50%) scale(${0.3 + t * 1.2})`
      },
      ease.out,
    )
  }

  async click(selector, s) {
    const el = document.querySelector(selector)
    if (!el) throw new Error(`demo: nothing matches ${selector}`)
    el.scrollIntoView({ block: 'nearest' })
    const r = el.getBoundingClientRect()
    await this.moveTo(r.left + r.width * 0.5, r.top + r.height * 0.55)
    el.classList.add('pressed')
    const press = this.press()
    await this.sleep(90)
    el.classList.remove('pressed')
    // Panels re-render after edits; click whatever now matches the selector.
    ;(document.querySelector(selector) || el).click()
    await this.idle()
    if (s.select === 'created') {
      const last = this.app.scene.objects.at(-1)
      if (last) this.app.select(last.id)
    }
    await press
    if (s.after) await this.sleep(s.after)
  }

  async pick(s) {
    const { app } = this
    const id = this.resolveId(s.pick)
    const component = s.vertex !== undefined || s.edge !== undefined
    const p = component ? app.viewport.anchorComponent(id, s) : app.viewport.anchor(id, s.face)
    if (!p) throw new Error(`demo: cannot pick ${s.pick}`)
    const screen = app.viewport.worldToScreen(p)
    const vr = $('viewport').getBoundingClientRect()
    await this.moveTo(vr.left + screen.x, vr.top + screen.y)
    this.press()
    if (app.mode === 'edit' && app.selected === id) {
      app.pickComponent({ vertex: s.vertex, edge: s.edge, face: s.face }, Boolean(s.shift))
    } else {
      app.select(id, s.face ?? null)
    }
    await this.sleep(140)
    if (s.after) await this.sleep(s.after)
  }

  async type(selector, text, cps = 28) {
    const el = document.querySelector(selector)
    if (!el) throw new Error(`demo: nothing matches ${selector}`)
    el.scrollIntoView({ block: 'nearest' })
    el.value = ''
    let typed = ''
    for (const ch of text) {
      typed += ch
      el.value = typed
      el.scrollTop = el.scrollHeight
      await this.sleep(1000 / cps)
    }
    el.dispatchEvent(new Event('change', { bubbles: true }))
  }

  /** Drag the timeline playhead to frame `s.scrub`. */
  async scrub(s) {
    const { app } = this
    const ruler = $('ruler')
    const r = ruler.getBoundingClientRect()
    const x = (f) => r.left + (app.frameToX(f) / 100) * r.width
    const y = r.top + r.height / 2
    const from = app.frame
    await this.moveTo(x(from), y)
    this.press()
    await app.animator.add('scrub', s.ms ?? 700, (t) => {
      const f = Math.round(from + (s.scrub - from) * t)
      app.setFrame(f)
      this.pos = { x: x(f), y }
      this.placeCursor()
    })
    if (s.after) await this.sleep(s.after)
  }

  /**
   * Drag a sculpt stroke through world points (projected to the screen, so
   * the brush follows the real surface). `shift` smooths, `ctrl` inverts.
   */
  async stroke(s) {
    const vp = this.app.viewport
    const pts = s.stroke.map((p) => vp.clientOf(p))
    const lengths = [0]
    for (let i = 1; i < pts.length; i++) lengths.push(lengths[i - 1] + Math.hypot(pts[i].x - pts[i - 1].x, pts[i].y - pts[i - 1].y))
    const total = lengths.at(-1)
    const at = (d) => {
      let i = 1
      while (i < pts.length - 1 && lengths[i] < d) i++
      const span = lengths[i] - lengths[i - 1] || 1
      const t = Math.min(1, Math.max(0, (d - lengths[i - 1]) / span))
      return { x: pts[i - 1].x + (pts[i].x - pts[i - 1].x) * t, y: pts[i - 1].y + (pts[i].y - pts[i - 1].y) * t }
    }
    await this.moveTo(pts[0].x, pts[0].y)
    vp.sculptMove(pts[0].x, pts[0].y)
    this.press()
    if (!vp.sculptDown(pts[0].x, pts[0].y, { shiftKey: s.shift, ctrlKey: s.ctrl })) throw new Error('demo: stroke starts off the object')
    await this.app.animator.add('stroke', s.ms ?? 900, (t) => {
      this.pos = at(total * t)
      vp.sculptMove(this.pos.x, this.pos.y)
      this.placeCursor()
    })
    vp.sculptUp()
    await this.idle()
    if (s.after) await this.sleep(s.after)
  }

  /** Pick an option of a <select> by its label, like a mouse would. */
  async choose(s) {
    const el = document.querySelector(s.choose)
    if (!el) throw new Error(`demo: nothing matches ${s.choose}`)
    el.scrollIntoView({ block: 'nearest' })
    const r = el.getBoundingClientRect()
    await this.moveTo(r.left + r.width * 0.6, r.top + r.height * 0.55)
    this.press()
    const option = [...el.options].find((o) => o.textContent === s.value)
    if (!option) throw new Error(`demo: no option ${s.value}`)
    el.value = option.value
    el.dispatchEvent(new Event('change', { bubbles: true }))
    await this.idle()
    if (s.after) await this.sleep(s.after)
  }

  /** Drag from one element to another with pointer events (node wires). */
  async wire(s) {
    const [fromSel, toSel] = s.wire
    const centre = (sel) => {
      const el = document.querySelector(sel)
      if (!el) throw new Error(`demo: nothing matches ${sel}`)
      const r = el.getBoundingClientRect()
      return [el, r.left + r.width / 2, r.top + r.height / 2]
    }
    const [from, x0, y0] = centre(fromSel)
    await this.moveTo(x0, y0)
    const press = this.press()
    from.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, clientX: x0, clientY: y0, button: 0 }))
    const [, x1, y1] = centre(toSel)
    const ms = s.ms ?? 700
    await this.app.animator.add(
      'cursor',
      ms,
      (t) => {
        this.pos = { x: x0 + (x1 - x0) * t, y: y0 + (y1 - y0) * t - Math.sin(Math.PI * t) * 30 }
        this.placeCursor()
        window.dispatchEvent(new PointerEvent('pointermove', { clientX: this.pos.x, clientY: this.pos.y }))
      },
      ease.inOut,
    )
    window.dispatchEvent(new PointerEvent('pointerup', { clientX: x1, clientY: y1 }))
    await press
    await this.idle()
    if (s.after) await this.sleep(s.after)
  }

  /** Press on the UV island holding `face` in the UV editor; drag it by `drag` (in UV units). */
  async uv(s) {
    const canvas = document.querySelector('#uv-editor:not([hidden]) .uv-canvas')
    if (!canvas) throw new Error('demo: the UV editor is not open')
    const id = this.resolveId(s.uv)
    const uvs = this.app.scene.objects.find((o) => o.id === id)?.mesh.uvs?.[s.face]
    if (!uvs) throw new Error(`demo: face ${s.face} of ${s.uv} has no UVs`)
    const [u, v] = uvs.reduce((a, p) => [a[0] + p[0] / uvs.length, a[1] + p[1] / uvs.length], [0, 0])
    const r = canvas.getBoundingClientRect()
    const at = (u, v) => [r.left + u * r.width, r.top + (1 - v) * r.height]
    const [x0, y0] = at(u, v)
    await this.moveTo(x0, y0)
    const press = this.press()
    canvas.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, clientX: x0, clientY: y0, button: 0 }))
    const [x1, y1] = at(u + (s.drag?.[0] ?? 0), v + (s.drag?.[1] ?? 0))
    if (s.drag) {
      await this.app.animator.add(
        'cursor',
        s.ms ?? 700,
        (t) => {
          this.pos = { x: x0 + (x1 - x0) * t, y: y0 + (y1 - y0) * t }
          this.placeCursor()
          window.dispatchEvent(new PointerEvent('pointermove', { clientX: this.pos.x, clientY: this.pos.y }))
        },
        ease.inOut,
      )
    }
    window.dispatchEvent(new PointerEvent('pointerup', { clientX: x1, clientY: y1 }))
    await press
    await this.idle()
    if (s.after) await this.sleep(s.after)
  }

  /** Drag the reach (IK) handle to world point `reach`, then let go. */
  async reach(s) {
    const vp = this.app.viewport
    if (!vp.ik || !vp.ikHandle.visible) throw new Error('demo: no reach handle to drag')
    const vr = $('viewport').getBoundingClientRect()
    const screen = (p) => {
      const q = vp.worldToScreen(p)
      return [vr.left + q.x, vr.top + q.y]
    }
    const from = vp.ikHandle.position.clone()
    const to = new THREE.Vector3(...s.reach)
    await this.moveTo(...screen(from))
    this.press()
    await this.app.animator.add(
      'cursor',
      s.ms ?? 900,
      (t) => {
        const p = from.clone().lerp(to, t)
        vp.dragIk(p)
        ;[this.pos.x, this.pos.y] = screen(p)
        this.placeCursor()
      },
      ease.inOut,
    )
    vp.commitIk()
    await this.idle()
    if (s.after) await this.sleep(s.after)
  }

  /**
   * Wait until the rendered preview (or with `final`, the Render panel)
   * holds `samples` samples per pixel.
   */
  async samples(s) {
    const preview = s.final ? this.app.finalRender : this.app.preview
    for (let i = 0; i < 4000 && preview.samples < s.samples; i++) await this.sleep(66)
    if (preview.samples < s.samples) throw new Error(`demo: the render stopped at ${preview.samples} samples`)
    if (s.after) await this.sleep(s.after)
  }

  /** Hand a file input a painted PNG, as if the user had picked it. */
  async upload(s) {
    const el = document.querySelector(s.upload)
    if (!el) throw new Error(`demo: nothing matches ${s.upload}`)
    const canvas = paint(s.paint)
    const blob = await this.clock.track(new Promise((resolve) => canvas.toBlob(resolve, 'image/png')))
    const files = new DataTransfer()
    files.items.add(new File([blob], s.name, { type: 'image/png' }))
    el.files = files.files
    el.dispatchEvent(new Event('change', { bubbles: true }))
    await this.idle()
    if (s.after) await this.sleep(s.after)
  }

  /** Drag a range input's thumb to `to`, then commit it like a mouse release. */
  /** Drag a number's label sideways (History scrubbing) to `s.to`. */
  async dragNumber(s) {
    const key = document.querySelector(s.drag)
    if (!key) throw new Error(`demo: nothing matches ${s.drag}`)
    key.scrollIntoView({ block: 'nearest' })
    const input = key.parentElement.querySelector('input')
    const from = Number(input.value)
    const r = key.getBoundingClientRect()
    const y = r.top + r.height / 2
    await this.moveTo(r.left + r.width / 2, y)
    this.press()
    const x0 = this.pos.x
    await this.app.animator.add('drag', s.ms ?? 1200, (t) => {
      input.value = String(Math.round((from + (s.to - from) * t) * 1000) / 1000)
      input.dispatchEvent(new Event('input', { bubbles: true }))
      this.pos = { x: x0 + t * 140, y }
      this.placeCursor()
    })
    input.dispatchEvent(new Event('change', { bubbles: true }))
    await this.idle()
    if (s.after) await this.sleep(s.after)
  }

  async slide(s) {
    const el = document.querySelector(s.slide)
    if (!el) throw new Error(`demo: nothing matches ${s.slide}`)
    el.scrollIntoView({ block: 'nearest' })
    const min = Number(el.min)
    const max = Number(el.max)
    const from = Number(el.value)
    const r = el.getBoundingClientRect()
    const x = (v) => r.left + 8 + ((v - min) / (max - min)) * (r.width - 16)
    const y = r.top + r.height / 2
    await this.moveTo(x(from), y)
    this.press()
    const label = el.parentElement.querySelector('b')
    await this.app.animator.add('slide', s.ms ?? 900, (t) => {
      const v = from + (s.to - from) * t
      el.value = String(v)
      if (label) label.textContent = String(Math.round(Number(el.value) * 100) / 100)
      // Live previews (bone turns) follow the thumb.
      el.dispatchEvent(new Event('input', { bubbles: true }))
      this.pos = { x: x(Number(el.value)), y }
      this.placeCursor()
    })
    el.dispatchEvent(new Event('change', { bubbles: true }))
    await this.idle()
    if (s.after) await this.sleep(s.after)
  }

  // -- agent replays ----------------------------------------------------------

  async chat(s) {
    const { app } = this
    app.replaying = true
    app.showTab('agent')
    const status = $('chat-status')
    $('chat-input').disabled = false
    status.textContent = ''
    $('batch-input').value = ''
    app.render()
    await this.type('#chat-input', s.chat, s.cps ?? 48)
    await this.sleep(250)
    const send = $('chat-send')
    const r = send.getBoundingClientRect()
    await this.moveTo(r.left + r.width / 2, r.top + r.height / 2)
    await this.press()
    status.textContent = 'Thinking…'
    await this.sleep(s.think ?? 700)
    const json = JSON.stringify({ commands: s.commands }, null, 1).replace(/\n\s*/g, ' ')
    status.textContent = 'Streaming commands…'
    const out = $('batch-input')
    out.value = ''
    const chunk = Math.max(6, Math.ceil(json.length / 30))
    for (let i = 0; i < json.length; i += chunk) {
      out.value = json.slice(0, i + chunk)
      out.scrollTop = out.scrollHeight
      await this.sleep(33)
    }
    status.textContent = 'Validating…'
    await this.sleep(150)
    const res = await app.run(s.commands, 'Agent')
    status.textContent = `Applied ${s.commands.length} commands as revision ${res.revision} · replay`
    $('chat-input').value = ''
    if (s.after) await this.sleep(s.after)
  }

  termLine(html) {
    const body = $('terminal-body')
    const line = document.createElement('div')
    line.innerHTML = html
    body.append(line)
    while (body.children.length > 14) body.firstChild.remove()
    return line
  }

  async mcp(s) {
    const { app } = this
    const term = $('terminal')
    if (s.clear) $('terminal-body').replaceChildren()
    if (Number(getComputedStyle(term).opacity) < 0.99 || term.style.display === 'none') await this.fade(term, 1, 250)
    for (const call of s.mcp) {
      if (call.say) {
        // The scripted agent's reasoning, shown as a comment line.
        const line = this.termLine(`<span class="t-say"></span>`)
        const el = line.querySelector('.t-say')
        for (let i = 1; i <= call.say.length; i += 3) {
          el.textContent = `# ${call.say.slice(0, i)}`
          await this.sleep(18)
        }
        el.textContent = `# ${call.say}`
        await this.sleep(call.after ?? 350)
        continue
      }
      const args = JSON.stringify(call.arguments ?? {})
      const shown = args.length > 46 ? `${args.slice(0, 44)}…` : args
      const line = this.termLine(`<span class="t-out">→</span> <span class="t-tool"></span> <span class="t-args"></span>`)
      const toolEl = line.querySelector('.t-tool')
      const argsEl = line.querySelector('.t-args')
      for (let i = 1; i <= call.tool.length; i += 2) {
        toolEl.textContent = call.tool.slice(0, i)
        await this.sleep(24)
      }
      toolEl.textContent = call.tool
      argsEl.textContent = shown
      await this.sleep(call.think ?? 260)
      const result = await this.callTool(call.tool, call.arguments ?? {})
      let summary
      const details = []
      if (result.isError) {
        let message = result.text
        try {
          message = JSON.parse(result.text).error || message
        } catch {}
        summary = `<span class="t-err">✗ ${escapeHtml(message.split('\n')[0])}</span>`
      }
      else if (call.tool === 'render_image') summary = `<span class="t-ok">✓</span> wrote ${escapeHtml(call.arguments.path)}`
      else if (result.image) {
        summary = `<span class="t-ok">✓</span> image · ${escapeHtml((call.arguments?.views ?? ['iso']).join(', '))}`
      }
      else {
        const data = JSON.parse(result.text)
        if (call.tool === 'get_scene') summary = `revision ${data.revision} · ${data.objects.length} objects`
        else if (call.tool === 'inspect_scene') {
          app.showChecks(data)
          summary = escapeHtml(data.summary)
          for (const issue of data.issues) details.push(`<span class="t-err">  ⚠ ${escapeHtml(issue.message)}</span>`)
        }
        else if (call.tool === 'propose_changes') summary = `proposed ${data.ids.length > 1 ? `${data.ids.length} options` : `#${data.ids[0]}`} · waiting for review`
        else if (call.tool === 'get_history') summary = `${data.total} steps · ${data.steps.map((x) => x.source).filter((v, i, a) => a.indexOf(v) === i).join(', ')}`
        else if (call.tool === 'revise_step') summary = `revised step ${call.arguments.step} · replayed · revision ${data.revision}`
        else if (call.tool === 'list_proposals') {
          const done = data.decided.slice(-4)
          summary = `${data.pending.length} pending · ${done.length} decided`
          for (const d of done) details.push(`<span class="t-say">  ${d.outcome === 'accepted' ? '✓' : '·'} ${escapeHtml(d.outcome)}: ${escapeHtml(d.title)}</span>`)
        }
        else if (data.created?.length > 6) summary = `revision ${data.revision} · created ${data.created.length} objects`
        else if (data.created?.length) summary = `revision ${data.revision} · created ${JSON.stringify(data.created)}`
        else summary = `revision ${data.revision}`
        summary = details.length && call.tool === 'inspect_scene' ? `<span class="t-err">⚠</span> ${summary}` : `<span class="t-ok">✓</span> ${summary}`
      }
      this.termLine(`<span class="t-in">←</span> ${summary}`)
      for (const line of details) {
        this.termLine(line)
        await this.sleep(120)
      }
      if (result.image) {
        const body = $('terminal-body')
        for (const old of body.querySelectorAll('.t-img')) old.parentElement.remove()
        while (body.children.length > 6) body.firstChild.remove()
        const line = this.termLine(`<img class="t-img" alt="render_view result">`)
        line.querySelector('img').src = result.image
        await this.clock.track(line.querySelector('img').decode().catch(() => {}))
      }
      if (['propose_changes', 'list_proposals', 'get_history'].includes(call.tool) && !result.isError) await app.refresh(true)
      if (!['get_scene', 'render_view', 'render_image', 'inspect_scene', 'propose_changes', 'list_proposals', 'get_history'].includes(call.tool) && !result.isError) {
        await app.refresh(true)
        app.log('MCP', call.tool === 'apply_commands' ? app.summarize(call.arguments.commands) : call.tool)
      }
      await this.sleep(call.after ?? 500)
    }
  }

  async callTool(tool, args) {
    // The recorder exposes a real `tatara --mcp` process here.
    if (window.tataraMcp) return this.clock.track(window.tataraMcp(tool, args))
    const { api } = this.app
    try {
      let data
      if (tool === 'get_scene') data = await this.clock.track(fetch('/api/context').then((r) => r.json()))
      else if (tool === 'inspect_scene') data = await api.inspect()
      else if (tool === 'apply_commands') data = await api.commands(args.commands, args.expected_revision, 'agent')
      else if (tool === 'get_history') data = await api.history()
      else if (tool === 'revise_step') data = await api.revise(args.step, args.commands)
      else if (tool === 'render_view') {
        const q = new URLSearchParams({ views: (args.views ?? ['iso']).join(','), size: String(args.size ?? 512) })
        if (args.object) q.set('object', args.object)
        const blob = await this.clock.track(fetch(`/api/render?${q}`).then((r) => r.blob()))
        const image = await this.clock.track(
          new Promise((resolve) => {
            const reader = new FileReader()
            reader.onload = () => resolve(reader.result)
            reader.readAsDataURL(blob)
          }),
        )
        return { text: '', image, isError: false }
      }
      else if (tool === 'propose_changes') data = await api.propose({ author: 'agent', ...args })
      else if (tool === 'list_proposals') data = await api.proposals()
      else if (tool === 'render_image') {
        const [w, h] = args.size ?? [1280, 720]
        const q = new URLSearchParams({ w: String(w), h: String(h), samples: String(args.samples ?? 128), view: args.view ?? 'iso' })
        for (const k of ['aperture', 'focus', 'frame']) if (args[k] !== undefined) q.set(k, String(args[k]))
        const res = await this.clock.track(fetch(`/api/render/image?${q}`))
        if (!res.ok) throw new Error(await res.text())
        const bytes = (await this.clock.track(res.arrayBuffer())).byteLength
        return { text: `wrote a ${w}x${h} render (${bytes} bytes) to ${args.path}`, isError: false }
      }
      else data = await api[tool]()
      return { text: JSON.stringify(data), isError: false }
    } catch (e) {
      return { text: e.message, isError: true }
    }
  }
}

/** Deterministic pictures for demos that upload an image. */
function paint(kind) {
  const canvas = document.createElement('canvas')
  canvas.width = 640
  canvas.height = 448
  const g = canvas.getContext('2d')
  const { width: w, height: h } = canvas
  if (kind !== 'sunset') throw new Error(`demo: no painting ${kind}`)
  const sky = g.createLinearGradient(0, 0, 0, h * 0.62)
  sky.addColorStop(0, '#1b2350')
  sky.addColorStop(0.45, '#7a3b78')
  sky.addColorStop(0.8, '#f0784a')
  sky.addColorStop(1, '#ffd27a')
  g.fillStyle = sky
  g.fillRect(0, 0, w, h)
  const sun = g.createRadialGradient(w * 0.62, h * 0.56, 4, w * 0.62, h * 0.56, 120)
  sun.addColorStop(0, '#fff6d0')
  sun.addColorStop(0.35, '#ffd27a')
  sun.addColorStop(1, 'rgba(255,170,90,0)')
  g.fillStyle = sun
  g.fillRect(0, 0, w, h)
  // Ridges, far to near, each darker.
  const ridges = [
    ['#5b3a6e', 0.5, 38, 0.011, 1.3],
    ['#3b2a55', 0.6, 46, 0.017, 4.1],
    ['#211a3a', 0.72, 30, 0.026, 2.2],
  ]
  for (const [color, base, amp, freq, phase] of ridges) {
    g.fillStyle = color
    g.beginPath()
    g.moveTo(0, h)
    for (let x = 0; x <= w; x += 4) {
      const y = h * base - amp * (Math.sin(x * freq + phase) * 0.6 + Math.sin(x * freq * 2.7 + phase * 3) * 0.4)
      g.lineTo(x, y)
    }
    g.lineTo(w, h)
    g.fill()
  }
  // A lake mirrors the sky below the last ridge.
  const lake = g.createLinearGradient(0, h * 0.8, 0, h)
  lake.addColorStop(0, '#f0985a')
  lake.addColorStop(1, '#3b2a55')
  g.fillStyle = lake
  g.fillRect(0, h * 0.82, w, h * 0.18)
  g.fillStyle = 'rgba(255,240,200,0.55)'
  for (let k = 0; k < 7; k++) g.fillRect(w * 0.62 - 60 + k * 9, h * 0.84 + k * 9, 120 - k * 18, 3)
  return canvas
}
