// Browser-only backend: the Rust modeling core compiled to WebAssembly.
//
// It answers the same `/api/...` requests as the native server, through the
// same Rust router, so the editor UI runs unchanged. `fetch` is wrapped for
// `/api/` URLs only; everything else goes to the network as usual.

const encoder = new TextEncoder()
const decoder = new TextDecoder()

async function bodyBytes(body) {
  if (body == null) return new Uint8Array()
  if (typeof body === 'string') return encoder.encode(body)
  if (body instanceof ArrayBuffer) return new Uint8Array(body)
  if (ArrayBuffer.isView(body)) return new Uint8Array(body.buffer, body.byteOffset, body.byteLength)
  if (body instanceof Blob) return new Uint8Array(await body.arrayBuffer())
  throw new Error('unsupported request body')
}

export async function installWasmBackend(url) {
  const response = await fetch(url)
  if (!response.ok) throw new Error(`could not load ${url}: ${response.status}`)
  const { instance } = await WebAssembly.instantiate(await response.arrayBuffer(), {})
  const x = instance.exports

  const put = (bytes) => {
    const ptr = x.tatara_alloc(bytes.length)
    new Uint8Array(x.memory.buffer, ptr, bytes.length).set(bytes)
    return [ptr, bytes.length]
  }
  const read = (ptr, len) => new Uint8Array(x.memory.buffer, ptr, len).slice()

  function request(method, path, body) {
    const args = [put(encoder.encode(method)), put(encoder.encode(path)), put(body)]
    try {
      const status = x.tatara_request(...args.flat())
      return {
        status,
        body: read(x.tatara_response_ptr(), x.tatara_response_len()),
        type: decoder.decode(read(x.tatara_response_type_ptr(), x.tatara_response_type_len())),
        disposition: decoder.decode(read(x.tatara_response_disposition_ptr(), x.tatara_response_disposition_len())),
      }
    } finally {
      for (const [ptr, len] of args) x.tatara_free(ptr, len)
    }
  }

  const networkFetch = window.fetch.bind(window)
  window.fetch = async (input, init = {}) => {
    const raw = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
    const url = new URL(raw, location.href)
    const match = url.origin === location.origin && url.pathname.match(/\/api(\/.*)$/)
    if (!match) return networkFetch(input, init)
    const method = (init.method || (input instanceof Request ? input.method : 'GET')).toUpperCase()
    const r = request(method, match[1] + url.search, await bodyBytes(init.body))
    const headers = { 'Content-Type': r.type || 'application/octet-stream' }
    if (r.disposition) headers['Content-Disposition'] = r.disposition
    return new Response(r.body, { status: r.status, headers })
  }
  return { request }
}
