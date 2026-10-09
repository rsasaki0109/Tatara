// The proposal tray: changes offered for review (by agents over MCP, or
// anyone through /api/proposals). Each card shows a render of the scene with
// the change applied and what it adds, changes and removes; hovering a card
// previews it in the viewport, Accept applies it as one undo step and ✕
// drops it. Variants of one request are alternatives: accepting one drops
// the rest.

const escapeHtml = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c])
const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`

export class ProposalTray {
  /**
   * `camera()` gives the thumbnails' camera as query parameters (eye,
   * target, fov); `onPreview(id | null)` shows a proposal in the viewport
   * (null: the scene again); `onDecide(id, accept)` accepts or rejects one.
   */
  constructor(el, { clock, camera, onPreview, onDecide }) {
    Object.assign(this, { el, clock, camera, onPreview, onDecide })
    this.pending = []
    this.version = -1
    this.revision = -1
    this.previewing = null
    this.pinned = null
    this.thumbs = new Map()
    el.addEventListener('click', (e) => this.click(e))
    el.addEventListener('mouseover', (e) => {
      const card = e.target.closest('[data-proposal]')
      if (card && !e.target.closest('button')) this.preview(Number(card.dataset.proposal))
    })
    el.addEventListener('mouseleave', () => this.preview(this.pinned))
  }

  /** Show the proposals from the core (`{version, pending}`) at `revision`. */
  set(proposals, revision) {
    if (!proposals) return
    if (proposals.version === this.version && revision === this.revision) return
    this.version = proposals.version
    this.revision = revision
    this.pending = proposals.pending
    const ids = new Set(this.pending.map((p) => p.id))
    if (this.pinned != null && !ids.has(this.pinned)) this.pinned = null
    if (this.previewing != null && !ids.has(this.previewing)) this.preview(null)
    for (const id of this.thumbs.keys()) if (!ids.has(id)) this.thumbs.delete(id)
    this.draw()
  }

  draw() {
    const list = this.pending
    this.el.hidden = list.length === 0
    if (!list.length) return void (this.el.innerHTML = '')
    const groups = new Set(list.filter((p) => p.group != null).map((p) => p.group))
    const head = groups.size ? 'Choose one' : 'Review'
    this.el.innerHTML = `<div class="pt-head"><b>Proposals</b><span class="muted small">${head} · hover to preview</span></div>
      ${list.map((p) => this.card(p)).join('')}`
    for (const p of list) this.thumb(p)
  }

  card(p) {
    const d = p.diff
    const chips = [
      d.added.length ? `<span class="pc-add" title="${escapeHtml(d.added.map((o) => o.name).join(', '))}">+${d.added.length}</span>` : '',
      d.changed.length ? `<span class="pc-change" title="${escapeHtml(d.changed.map((o) => `${o.name}: ${o.parts.join(', ')}`).join('\n'))}">~${d.changed.length}</span>` : '',
      d.removed.length ? `<span class="pc-remove" title="${escapeHtml(d.removed.map((o) => o.name).join(', '))}">−${d.removed.length}</span>` : '',
      d.scene.length ? `<span class="pc-change">${escapeHtml(d.scene.join(', '))}</span>` : '',
    ].join('')
    const cls = ['proposal-card', p.conflict ? 'conflict' : '', p.id === this.previewing ? 'previewing' : '', p.id === this.pinned ? 'pinned' : '']
    return `<div class="${cls.join(' ')}" data-proposal="${p.id}">
      <img class="pc-thumb" alt="">
      <div class="pc-body">
        <div class="pc-title" title="${escapeHtml(p.note ?? p.title)}">${escapeHtml(p.title)}</div>
        <div class="pc-meta"><span class="pc-author">${escapeHtml(p.author)}</span>${chips || '<span class="muted">no change</span>'}</div>
        ${p.conflict ? `<div class="pc-conflict" title="${escapeHtml(p.conflict)}">No longer applies</div>` : `<div class="pc-note muted">${escapeHtml(p.note ?? plural(p.commands, 'command'))}</div>`}
      </div>
      <div class="pc-actions">
        <button class="small-btn primary" data-pc="accept" ${p.conflict ? 'disabled' : ''} title="Apply it (one undo step)">Accept</button>
        <button class="small-btn" data-pc="reject" title="Drop it">✕</button>
      </div>
    </div>`
  }

  /** A render of the proposed scene, refreshed when the scene moves on. */
  thumb(p) {
    const key = `${p.id}:${this.revision}:${JSON.stringify(p.diff)}`
    const img = this.el.querySelector(`[data-proposal="${p.id}"] .pc-thumb`)
    const known = this.thumbs.get(p.id)
    if (known?.key === key) {
      if (known.url) img.src = known.url
      return
    }
    const entry = { key, url: known?.url ?? null }
    this.thumbs.set(p.id, entry)
    if (entry.url) img.src = entry.url
    this.clock.track(
      fetch(`/api/proposal/render?${new URLSearchParams({ id: p.id, ...this.camera(), w: 240, h: 180, samples: 8 })}`)
        .then((r) => (r.ok ? r.blob() : null))
        .then(async (blob) => {
          if (!blob || this.thumbs.get(p.id) !== entry) return
          const url = URL.createObjectURL(blob)
          const probe = new Image()
          probe.src = url
          await probe.decode().catch(() => {})
          entry.url = url
          const now = this.el.querySelector(`[data-proposal="${p.id}"] .pc-thumb`)
          if (now) now.src = url
        })
        .catch(() => {}),
    )
  }

  click(e) {
    const card = e.target.closest('[data-proposal]')
    if (!card) return
    const id = Number(card.dataset.proposal)
    const action = e.target.closest('[data-pc]')?.dataset.pc
    if (action) {
      if (action === 'accept' || action === 'reject') {
        if (this.pinned === id) this.pinned = null
        this.preview(null)
        this.onDecide(id, action === 'accept')
      }
      return
    }
    // A click pins the preview (another click lets it go).
    this.pinned = this.pinned === id ? null : id
    this.preview(this.pinned ?? id)
  }

  preview(id) {
    if (id === this.previewing) return
    this.previewing = id
    for (const card of this.el.querySelectorAll('[data-proposal]')) {
      card.classList.toggle('previewing', Number(card.dataset.proposal) === id)
      card.classList.toggle('pinned', Number(card.dataset.proposal) === this.pinned)
    }
    this.onPreview(id)
  }
}
