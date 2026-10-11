// Local browser snapshots retain the same validated project used by Save/Open.
// Each document owns a record so independent tabs never share a write key.
const DATABASE = 'tatara-recovery'
const STORE = 'sessions'
const KEEP = 5

function complete(tx) {
  return new Promise((resolve, reject) => {
    tx.oncomplete = resolve
    tx.onerror = tx.onabort = () => reject(tx.error || new Error('Local storage failed'))
  })
}
function result(request) {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result)
    request.onerror = () => reject(request.error)
  })
}

export class Recovery {
  constructor({api, revision, restored}) {
    Object.assign(this, {api, revision, restored})
    this.id = crypto.randomUUID?.() ?? Array.from(crypto.getRandomValues(new Uint8Array(16)), b => b.toString(16).padStart(2, '0')).join('')
    this.latest = null
    this.saved = null
    this.busy = false
    this.timer = null
    this.records = []
    this.badge = document.createElement('button')
    this.badge.className = 'recovery-status'
    this.badge.type = 'button'
    this.badge.textContent = 'Local recovery'
    this.badge.addEventListener('click', () => this.show())
    document.querySelector('.statusbar').append(this.badge)
    this.panel = document.createElement('section')
    this.panel.className = 'recovery-panel'
    this.panel.setAttribute('aria-label', 'Browser recovery')
    this.panel.hidden = true
    this.panel.innerHTML = '<h3>Browser recovery</h3><p class="recovery-message"></p><select aria-label="Saved browser session"></select><div><button class="primary" data-recover="restore">Restore</button><button data-recover="discard">Discard</button><button data-recover="close">Close</button></div>'
    document.body.append(this.panel)
    this.panel.querySelector('[data-recover=close]').onclick = () => { this.panel.hidden = true }
    this.panel.querySelector('[data-recover=restore]').onclick = () => this.restore()
    this.panel.querySelector('[data-recover=discard]').onclick = () => this.discard()
    document.addEventListener('visibilitychange', () => {
      if (document.hidden) this.flush()
    })
  }

  async start() {
    try {
      const request = indexedDB.open(DATABASE, 1)
      request.onupgradeneeded = () => {
        const store = request.result.createObjectStore(STORE, {keyPath:'id'})
        store.createIndex('updatedAt', 'updatedAt')
      }
      this.db = await result(request)
      await this.read()
      this.status('ready', this.records.length ? 'Recovery available' : 'Local recovery ready')
      if (this.records.length) await this.show(false)
    } catch (e) { this.failed(e) }
  }

  status(state, text) {
    this.badge.dataset.state = state
    this.badge.textContent = text
    this.badge.title = text
    this.badge.setAttribute('aria-label', `Browser recovery: ${text}`)
  }
  failed(error) {
    this.error = error.name === 'QuotaExceededError' ? 'Browser storage is full.' : 'Browser storage is unavailable.'
    this.status('error', 'Local recovery unavailable')
  }
  async read() {
    const tx = this.db.transaction(STORE, 'readonly')
    const finished = complete(tx)
    const [records] = await Promise.all([result(tx.objectStore(STORE).getAll()), finished])
    this.records = records.filter(r => r.id !== this.id).sort((a,b) => b.updatedAt-a.updatedAt)
  }

  schedule(revision) {
    if (!this.db || revision === 0 || revision === this.latest) return
    this.latest = revision
    this.status('pending', 'Saving locally…')
    clearTimeout(this.timer)
    this.timer = setTimeout(() => this.flush(), 400)
  }

  async flush() {
    clearTimeout(this.timer)
    if (!this.db || this.busy || this.latest == null || this.latest === this.saved) return
    this.busy = true
    const revision = this.latest
    try {
      const project = await this.api.project()
      const tx = this.db.transaction(STORE, 'readwrite')
      const finished = complete(tx)
      const store = tx.objectStore(STORE)
      store.put({id:this.id, updatedAt:Date.now(), project})
      // Prune keys in the same transaction without cloning old image payloads.
      let count = 0
      store.index('updatedAt').openKeyCursor(null, 'prev').onsuccess = e => {
        const cursor = e.target.result
        if (!cursor) return
        if (cursor.primaryKey !== this.id && ++count >= KEEP) store.delete(cursor.primaryKey)
        cursor.continue()
      }
      await finished
      this.saved = revision
      this.error = null
      this.status(this.latest === revision ? 'saved' : 'pending', this.latest === revision ? 'Saved locally' : 'Saving locally…')
    } catch (e) { this.failed(e) }
    finally {
      this.busy = false
      if (this.latest !== revision) this.timer = setTimeout(() => this.flush(), 0)
    }
  }

  async show(refresh = true) {
    if (refresh && this.db) {
      try { await this.read() } catch (e) { this.failed(e) }
    }
    const select = this.panel.querySelector('select')
    select.replaceChildren()
    for (const record of this.records) {
      const option = document.createElement('option')
      option.value = record.id
      option.textContent = `${new Date(record.updatedAt).toLocaleString()} · ${record.project?.steps?.length ?? '?'} editable steps`
      select.append(option)
    }
    this.panel.querySelector('.recovery-message').textContent = this.error
      ? `${this.error} Save a project file to keep your work.`
      : this.records.length ? 'Restore a previous browser session as one Undo. Your current scene stays until you choose Restore.' : 'Edits are saved locally after they settle. Save a project file for a separate backup.'
    select.hidden = !this.records.length
    for (const action of ['restore','discard']) this.panel.querySelector(`[data-recover=${action}]`).disabled = !this.records.length
    this.panel.hidden = false
  }
  chosen() {
    return this.records.find(r => r.id === this.panel.querySelector('select').value)
  }
  async restore() {
    const record = this.chosen()
    if (!record) return
    try {
      const expected = this.revision()
      await this.api.putProject(record.project, expected)
      await this.restored()
      this.panel.hidden = true
    } catch (e) {
      this.panel.querySelector('.recovery-message').textContent = `Could not restore: ${e.message}`
    }
  }
  async discard() {
    const record = this.chosen()
    if (!record) return
    try {
      const tx = this.db.transaction(STORE, 'readwrite')
      const finished = complete(tx)
      tx.objectStore(STORE).delete(record.id)
      await finished
      await this.read()
      if (this.latest == null) this.status('ready', this.records.length ? 'Recovery available' : 'Local recovery ready')
      await this.show(false)
    } catch (e) { this.failed(e); this.show() }
  }
}
