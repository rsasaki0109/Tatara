// Shared-session presence is ephemeral. It never enters the scene or undo.

const COLORS = ['#77c9ff', '#f1a4c9', '#a5dc8c', '#eac478', '#baa7ff']
export const peerColor = (id) => COLORS[[...id].reduce((n, c) => (n * 31 + c.charCodeAt(0)) >>> 0, 0) % COLORS.length]

export class Collaboration {
  constructor({ api, viewport, scene, selected, onIdle, capture }) {
    Object.assign(this, { api, viewport, scene, selected, onIdle, capture })
    const id = capture ? 'local-demo' : crypto.randomUUID()
    this.actor = { id, name: capture ? 'You' : `Guest ${id.slice(0, 4)}` }
    this.peers = []
    this.version = -1
    this.epoch = 0
    this.cursor = null
    this.interacting = false
    this.revision = null
    this.busy = false
    this.last = ''
    this.lastSent = 0
    this.panel = document.getElementById('collaboration')
    this.panel.hidden = false
    this.panel.innerHTML = '<label class="session-name">You <input id="session-name" aria-label="Your session name" maxlength="64"></label><span id="session-status">Connecting…</span><div id="session-peers"></div>'
    const name = this.panel.querySelector('input')
    name.value = this.actor.name
    name.addEventListener('change', () => {
      const value = name.value.trim()
      if (!value || new TextEncoder().encode(value).length > 64) { name.value = this.actor.name; return }
      this.actor.name = value
      this.send(true)
    })
    this.overlay = document.createElement('div')
    this.overlay.className = 'peer-cursors'
    viewport.el.append(this.overlay)
    viewport.el.addEventListener('pointermove', (e) => {
      const r = viewport.el.getBoundingClientRect()
      this.cursor = [Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)), Math.max(0, Math.min(1, (e.clientY - r.top) / r.height))]
      this.send()
    })
    viewport.el.addEventListener('pointerleave', () => { this.cursor = null; this.send() })
    // Freeze the revision at the start of an interaction, rather than
    // committing an old gesture against a freshly fetched revision.
    document.addEventListener('pointerdown', (e) => {
      if (!e.target.closest('#viewport, #properties, #history, #toolbar')) return
      this.pointerActive = true
      if (!this.interacting) this.revision = scene().revision
      this.interacting = true
      this.send(true)
    })
    const end = () => {
      if (!this.interacting) return
      this.interacting = false
      this.revision = null
      this.send(true)
      onIdle()
    }
    document.addEventListener('pointerup', () => queueMicrotask(() => {
      this.pointerActive = false
      if (!document.activeElement?.matches('#properties input, #history input, #history textarea')) end()
    }))
    document.addEventListener('pointercancel', end)
    document.addEventListener('focusin', (e) => {
      if (!e.target.matches('#properties input, #history input, #history textarea')) return
      if (!this.interacting) this.revision = scene().revision
      this.interacting = true
      this.send(true)
    })
    document.addEventListener('focusout', () => queueMicrotask(() => {
      if (!this.pointerActive && !document.activeElement?.matches('#properties input, #history input, #history textarea')) end()
    }))
    window.addEventListener('blur', end)
    window.addEventListener('pagehide', () => {
      fetch('/api/presence', { method: 'DELETE', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ id: this.actor.id }), keepalive: true }).catch(() => {})
    })
    if (!capture) this.timer = setInterval(() => this.send(), 250)
  }

  payload() {
    return { actor: this.actor, cursor: this.cursor, selection: this.selected() == null ? [] : [this.selected()], camera: { eye: this.viewport.camera.position.toArray(), target: this.viewport.controls.target.toArray(), fov: this.viewport.camera.fov }, editing: this.interacting && this.selected() != null ? [this.selected()] : [] }
  }

  async send(force = false) {
    if (this.busy) return
    const p = this.payload()
    const text = JSON.stringify(p)
    const now = performance.now()
    if (!force && (now - this.lastSent < 150 || (text === this.last && now - this.lastSent < 5000))) return
    this.busy = true
    const epoch = this.epoch
    try {
      const data = await this.api.updatePresence(p)
      if (epoch !== this.epoch) return
      this.set(data)
      this.last = text
      this.lastSent = now
      document.getElementById('session-status').textContent = `${this.peers.length + 1} in session`
    } catch {
      document.getElementById('session-status').textContent = 'Presence offline · retrying'
    } finally { this.busy = false }
  }

  reset() {
    this.epoch++
    this.version = -1
    this.last = ''
    this.lastSent = 0
    this.set({ version: -1, peers: [] })
  }

  async load() {
    const epoch = this.epoch
    try {
      const data = await this.api.presence()
      if (epoch === this.epoch) this.set(data)
    } catch {}
  }

  set(data) {
    if (data.version < this.version) return
    this.version = data.version
    this.peers = data.peers.filter((p) => p.actor.id !== this.actor.id)
    const list = document.getElementById('session-peers')
    list.replaceChildren()
    this.overlay.replaceChildren()
    for (const p of this.peers) {
      const color = peerColor(p.actor.id)
      const row = document.createElement('div')
      row.className = 'session-peer'
      row.dataset.peer = p.actor.id
      row.style.setProperty('--peer-color', color)
      const label = document.createElement('span')
      const objects = p.selection.map((id) => this.scene().objects.find((o) => o.id === id)?.name).filter(Boolean)
      label.textContent = `${p.actor.name}${p.editing.length ? ' · editing' : objects.length ? ` · ${objects.join(', ')}` : ''}`
      row.append(label)
      if (p.camera) {
        const follow = document.createElement('button')
        follow.className = 'small-btn'
        follow.textContent = 'View camera'
        follow.addEventListener('click', () => {
          this.viewport.camera.position.fromArray(p.camera.eye)
          this.viewport.controls.target.fromArray(p.camera.target)
          this.viewport.camera.fov = p.camera.fov
          this.viewport.camera.updateProjectionMatrix()
          this.viewport.controls.update()
          this.send(true)
        })
        row.append(follow)
      }
      list.append(row)
      if (p.cursor) {
        const cursor = document.createElement('div')
        cursor.className = 'peer-cursor'
        cursor.dataset.peer = p.actor.id
        cursor.style.cssText = `left:${p.cursor[0] * 100}%;top:${p.cursor[1] * 100}%;--peer-color:${color}`
        cursor.textContent = `➤ ${p.actor.name}`
        this.overlay.append(cursor)
      }
    }
    document.getElementById('session-status').textContent = `${this.peers.length + 1} in session`
    this.viewport.setPresence(this.peers.map((p) => ({ ...p, color: peerColor(p.actor.id) })))
    this.markOutliner()
  }

  markOutliner() {
    for (const el of document.querySelectorAll('#outliner [data-id]')) {
      const id = Number(el.dataset.id)
      const peer = this.peers.find((p) => p.selection.includes(id))
      const editor = this.peers.find((p) => p.editing.includes(id))
      el.classList.toggle('peer-selected', Boolean(peer))
      el.classList.toggle('peer-editing', Boolean(editor))
      el.style.setProperty('--peer-color', peerColor((editor ?? peer)?.actor.id ?? ''))
      el.title = editor ? `${editor.actor.name} is editing · advisory lock` : peer ? `${peer.actor.name} selected this` : ''
    }
  }
}
