export class ApiError extends Error {
  constructor(message, status, data) {
    super(message)
    this.status = status
    this.data = data
  }
}

export function createApi(clock) {
  const request = (method, path, body) =>
    clock.track(
      fetch(`/api${path}`, {
        method,
        headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
        body: body === undefined ? undefined : JSON.stringify(body),
      }).then(async (res) => {
        const data = await res.json().catch(() => ({}))
        if (!res.ok) throw new ApiError(data.error || res.statusText, res.status, data)
        return data
      }),
    )

  return {
    state: () => request('GET', '/state'),
    scene: () => request('GET', '/scene'),
    commands: (commands, expected_revision, source) => request('POST', '/commands', { commands, expected_revision, source }),
    putScene: (scene) => request('PUT', '/scene', scene),
    undo: () => request('POST', '/undo'),
    redo: () => request('POST', '/redo'),
    reset: () => request('POST', '/reset'),
    inspect: () => request('GET', '/inspect'),
    chat: (prompt) => request('POST', '/chat', { prompt }),
    history: () => request('GET', '/history?limit=200'),
    schema: () => request('GET', '/schema'),
    previewRevision: (step, commands) => request('POST', '/history/preview', { step, commands }),
    revise: (step, commands) => request('POST', '/history/revise', { step, commands }),
    proposals: () => request('GET', '/proposals'),
    propose: (proposal) => request('POST', '/proposals', proposal),
    proposal: (id) => request('GET', `/proposal?id=${id}`),
    acceptProposal: (id) => request('POST', `/proposal/accept?id=${id}`),
    rejectProposal: (id) => request('POST', `/proposal/reject?id=${id}`),
    importModel: (bytes) =>
      clock.track(
        fetch('/api/import', { method: 'POST', body: bytes }).then(async (res) => {
          const data = await res.json().catch(() => ({}))
          if (!res.ok) throw new ApiError(data.error || res.statusText, res.status, data)
          return data
        }),
      ),
  }
}
