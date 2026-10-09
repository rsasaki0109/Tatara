// The History tab: the batches that made the scene, oldest first, as an
// editable recipe. Open a step to change any of its values; the scene
// replays from there (previewed live while you type or drag a label to
// scrub a number), and Apply commits it as one undo step. Steps after it
// follow: something placed on a table that grows ends up on the new top.

const escapeHtml = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c])
const isColor = (v) => typeof v === 'string' && /^#[0-9a-f]{6}$/i.test(v)
const isVector = (v) => Array.isArray(v) && v.length >= 2 && v.length <= 4 && v.every((x) => typeof x === 'number')
const fmt = (v) => String(Math.round(v * 1e4) / 1e4)

export class HistoryPanel {
  /**
   * `summarize(commands)` labels a step; `onPreview(scene | null, step)`
   * shows a replayed scene (null: the scene again); `onApplied()` follows a
   * committed revision; `onError(message)` reports one that failed.
   */
  constructor(el, { api, summarize, onPreview, onApplied, onError }) {
    Object.assign(this, { el, api, summarize, onPreview, onApplied, onError })
    this.steps = []
    this.open = null
    this.draft = null
    this.previewing = false
    this.timer = null
    this.asking = null
    el.addEventListener('click', (e) => this.click(e))
    el.addEventListener('input', (e) => this.edit(e))
    el.addEventListener('change', (e) => this.edit(e))
    el.addEventListener('pointerdown', (e) => this.scrub(e))
  }

  /** Fetch the history again (keeps an open step if it is still there). */
  async load() {
    // The command schema lists the values a step could set but doesn't.
    this.schema ??= this.api
      .schema()
      .then((s) => {
        const defs = s.$defs ?? s.definitions ?? {}
        const variants = new Map((defs.Command?.oneOf ?? []).map((v) => [v.properties?.op?.const ?? v.properties?.op?.enum?.[0], v]))
        return { defs, variants }
      })
      .catch(() => ({ defs: {}, variants: new Map() }))
    this.known = await this.schema
    const data = await this.api.history().catch(() => null)
    if (!data) return
    this.data = data
    this.steps = data.steps
    if (this.open != null && !this.steps.some((s) => s.step === this.open)) this.close()
    this.draw()
  }

  draw() {
    const d = this.data
    if (!d) return
    const intro = `<div class="hs-intro muted small">${d.total} step${d.total === 1 ? '' : 's'}${d.from_loaded_scene ? ' after the opened scene' : ''} · open one to change it; later steps replay</div>`
    if (!this.steps.length) {
      this.el.innerHTML = `${intro}<div class="empty-props"><p class="muted">Every edit you or an agent makes is recorded here as a step you can change later.</p></div>`
      return
    }
    this.el.innerHTML = `${intro}<ol class="hs-list">${this.steps.map((s) => this.row(s)).join('')}</ol>`
    if (this.open != null) this.el.querySelector(`[data-step="${this.open}"]`)?.scrollIntoView({ block: 'nearest' })
  }

  row(s) {
    const open = s.step === this.open
    const label = escapeHtml(this.summarize(s.commands))
    const head = `<div class="hs-head" data-hs-open="${s.step}"><span class="hs-n">${s.step}</span><span class="hs-what">${label}</span><span class="hs-src">${escapeHtml(s.actor ? `${s.actor.name} · ${s.source}` : s.source)}${s.rebased_from != null ? ' · combined' : ''}</span></div>`
    if (!open) return `<li class="hs-step" data-step="${s.step}">${head}</li>`
    const commands = this.draft ?? s.commands
    const body = s.editable
      ? commands.map((c, i) => `<div class="hs-cmd"><div class="hs-op">${escapeHtml(c.op)}</div>${this.fields(c, [i])}${this.unset(c, i)}</div>`).join('')
      : '<p class="muted small">Steps that add image data can’t be changed here.</p>'
    return `<li class="hs-step open" data-step="${s.step}">${head}
      <div class="hs-body">${body}
        <div class="hs-error" id="hs-error"></div>
        <div class="hs-actions">
          <button class="small-btn" data-hs="cancel">Cancel</button>
          <button class="small-btn primary" data-hs="apply" ${s.editable ? '' : 'disabled'}>Apply</button>
        </div>
      </div></li>`
  }

  /** Inputs for every value of a command (`path` locates it in the draft). */
  fields(value, path) {
    const out = []
    for (const [key, v] of Object.entries(value)) {
      if (key === 'op' && path.length === 1) continue
      const p = JSON.stringify([...path, key]).replace(/"/g, '&quot;')
      const name = `<span class="hs-key" ${typeof v === 'number' ? `data-scrub="${p}" title="Drag to change"` : ''}>${escapeHtml(key)}</span>`
      if (typeof v === 'number') out.push(`<label class="hs-field">${name}<input type="number" step="any" data-path="${p}" value="${fmt(v)}"></label>`)
      else if (typeof v === 'boolean') out.push(`<label class="hs-field">${name}<input type="checkbox" data-path="${p}" ${v ? 'checked' : ''}></label>`)
      else if (isColor(v)) out.push(`<label class="hs-field">${name}<input type="color" data-path="${p}" value="${v}"></label>`)
      else if (typeof v === 'string') out.push(`<label class="hs-field">${name}<input type="text" data-path="${p}" value="${escapeHtml(v)}"></label>`)
      else if (isVector(v)) {
        const inputs = v.map((x, i) => `<input type="number" step="any" data-path="${JSON.stringify([...path, key, i]).replace(/"/g, '&quot;')}" value="${fmt(x)}">`).join('')
        out.push(`<label class="hs-field vec"><span class="hs-key">${escapeHtml(key)}</span><span class="hs-vec">${inputs}</span></label>`)
      } else if (v && typeof v === 'object' && !Array.isArray(v)) {
        out.push(`<div class="hs-group"><div class="hs-key">${escapeHtml(key)}</div>${this.fields(v, [...path, key])}</div>`)
      } else if (v != null) {
        out.push(`<label class="hs-field json">${name}<textarea rows="2" data-json="${p}" spellcheck="false">${escapeHtml(JSON.stringify(v))}</textarea></label>`)
      }
    }
    return out.join('')
  }

  /** Buttons for the values a command could set but doesn't. */
  unset(command, i) {
    const variant = this.known?.variants.get(command.op)
    if (!variant) return ''
    const missing = Object.entries(variant.properties ?? {}).filter(([k, p]) => k !== 'op' && command[k] == null && this.blank(k, p) !== undefined)
    if (!missing.length) return ''
    return `<div class="hs-unset">${missing.map(([k, p]) => `<button class="chip" data-hs-add="${escapeHtml(JSON.stringify([i, k]))}" title="${escapeHtml(p.description ?? '')}">+ ${escapeHtml(k)}</button>`).join('')}</div>`
  }

  /** A starting value for an unset property (undefined: not offered). */
  blank(key, prop) {
    const types = [prop.type].flat()
    const ref = prop.$ref ?? prop.anyOf?.find((x) => x.$ref)?.$ref
    if (ref) {
      const def = this.known.defs[ref.split('/').pop()]
      const options = def?.enum ?? def?.oneOf?.map((o) => o.const ?? o.enum?.[0]).filter((o) => typeof o === 'string')
      return options?.length ? options[0] : undefined
    }
    if (types.includes('number') || types.includes('integer')) return /scale|count|levels|strength|factor|size/.test(key) ? 1 : 0
    if (types.includes('boolean')) return false
    if (types.includes('string')) return /color|emissive/.test(key) ? '#ffffff' : undefined
    if (types.includes('array') && prop.items?.type === 'number') return Array(prop.minItems ?? 3).fill(0)
    return undefined
  }

  click(e) {
    const add = e.target.closest('[data-hs-add]')
    if (add && this.draft) {
      const [i, key] = JSON.parse(add.dataset.hsAdd)
      this.draft[i][key] = this.blank(key, this.known.variants.get(this.draft[i].op).properties[key])
      this.draw()
      this.el.querySelector(`[data-path="${CSS.escape(JSON.stringify([i, key]))}"]`)?.focus()
      return
    }
    const head = e.target.closest('[data-hs-open]')
    if (head) {
      const step = Number(head.dataset.hsOpen)
      if (step === this.open) this.close()
      else {
        this.close()
        this.open = step
        this.baseRevision = this.data.revision
        this.draft = structuredClone(this.steps.find((s) => s.step === step).commands)
      }
      this.draw()
      return
    }
    const action = e.target.closest('[data-hs]')?.dataset.hs
    if (action === 'cancel') {
      this.close()
      this.draw()
    }
    if (action === 'apply') this.apply()
  }

  close() {
    clearTimeout(this.timer)
    this.open = null
    this.draft = null
    if (this.previewing) {
      this.previewing = false
      this.onPreview(null)
    }
  }

  /** Write an input's value into the draft and preview the result. */
  edit(e) {
    const t = e.target
    if (!this.draft || (!t.dataset.path && !t.dataset.json)) return
    try {
      const path = JSON.parse(t.dataset.path ?? t.dataset.json)
      let value
      if (t.dataset.json) value = JSON.parse(t.value)
      else if (t.type === 'checkbox') value = t.checked
      else if (t.type === 'number') {
        value = Number(t.value)
        if (t.value === '' || !Number.isFinite(value)) return
      } else value = t.value
      let at = this.draft
      for (const k of path.slice(0, -1)) at = at[k]
      at[path.at(-1)] = value
    } catch {
      return this.error('Not valid JSON')
    }
    clearTimeout(this.timer)
    this.timer = setTimeout(() => this.preview(), 60)
  }

  /** Drag a number's label sideways to change it. */
  scrub(e) {
    const key = e.target.closest('[data-scrub]')
    if (!key) return
    const input = key.parentElement.querySelector('input')
    const start = Number(input.value)
    const step = Math.max(Math.abs(start) * 0.01, 0.01)
    const x0 = e.clientX
    e.preventDefault()
    key.setPointerCapture?.(e.pointerId)
    const move = (m) => {
      input.value = fmt(start + Math.round((m.clientX - x0) / 2) * step)
      input.dispatchEvent(new Event('input', { bubbles: true }))
    }
    const up = () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', up)
    }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', up)
  }

  async preview() {
    if (!this.draft || this.open == null) return
    // One request at a time; the latest draft wins.
    if (this.asking) return void (this.again = true)
    const step = this.open
    this.asking = this.api.previewRevision(step, this.draft)
    try {
      const r = await this.asking
      if (this.open !== step) return
      this.error('')
      this.previewing = true
      this.onPreview(r.scene, step)
    } catch (err) {
      this.error(err.message)
    } finally {
      this.asking = null
      if (this.again) {
        this.again = false
        this.preview()
      }
    }
  }

  async apply() {
    if (!this.draft || this.open == null) return
    clearTimeout(this.timer)
    try {
      await this.api.revise(this.open, this.draft, this.baseRevision)
      const step = this.open
      this.previewing = false
      this.open = null
      this.draft = null
      await this.onApplied(step)
    } catch (err) {
      this.error(err.message)
      this.onError(err.message)
    }
  }

  error(message) {
    const el = this.el.querySelector('#hs-error')
    if (el) el.textContent = message
  }
}
