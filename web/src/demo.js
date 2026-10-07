// Scripted tool workflows. Every step drives the real editor: toolbar clicks,
// face picks and command batches all go through the same Rust command API.
// Timing uses the shared clock, so the capture recorder can step it frame by
// frame and produce identical recordings every run.

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
      app.showTab(scenario.tab || 'properties')
      if (scenario.setup) await app.api.commands(scenario.setup)
      await app.refresh(false)
      const vp = app.viewport
      vp.spinRate = 0
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

  /** Drag a range input's thumb to `to`, then commit it like a mouse release. */
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
      if (result.isError) {
        let message = result.text
        try {
          message = JSON.parse(result.text).error || message
        } catch {}
        summary = `<span class="t-err">✗ ${escapeHtml(message.split('\n')[0])}</span>`
      }
      else if (result.image) {
        summary = `<span class="t-ok">✓</span> image · ${escapeHtml((call.arguments?.views ?? ['iso']).join(', '))}`
      }
      else {
        const data = JSON.parse(result.text)
        if (call.tool === 'get_scene') summary = `revision ${data.revision} · ${data.objects.length} objects`
        else if (data.created?.length) summary = `revision ${data.revision} · created ${JSON.stringify(data.created)}`
        else summary = `revision ${data.revision}`
        summary = `<span class="t-ok">✓</span> ${summary}`
      }
      this.termLine(`<span class="t-in">←</span> ${summary}`)
      if (result.image) {
        const body = $('terminal-body')
        for (const old of body.querySelectorAll('.t-img')) old.parentElement.remove()
        while (body.children.length > 6) body.firstChild.remove()
        const line = this.termLine(`<img class="t-img" alt="render_view result">`)
        line.querySelector('img').src = result.image
        await this.clock.track(line.querySelector('img').decode().catch(() => {}))
      }
      if (call.tool !== 'get_scene' && call.tool !== 'render_view' && !result.isError) {
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
      else if (tool === 'apply_commands') data = await api.commands(args.commands, args.expected_revision)
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
      else data = await api[tool]()
      return { text: JSON.stringify(data), isError: false }
    } catch (e) {
      return { text: e.message, isError: true }
    }
  }
}
