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
    commands: (commands, expected_revision) => request('POST', '/commands', { commands, expected_revision }),
    putScene: (scene) => request('PUT', '/scene', scene),
    undo: () => request('POST', '/undo'),
    redo: () => request('POST', '/redo'),
    reset: () => request('POST', '/reset'),
    chat: (prompt) => request('POST', '/chat', { prompt }),
  }
}
