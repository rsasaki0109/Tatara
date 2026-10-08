// Brush strokes for the live preview while dragging. Mirrors src/sculpt.rs;
// when the stroke ends the same points go to the engine as one `sculpt`
// command, and its result replaces the preview.

export const SPACING = 0.2
const DAB_HEIGHT = 0.15
const REFINE_PASSES = 4
const SPLIT_RATIO = 4 / 3
const MAX_FACES = 250000

const sub = (a, b) => [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
const add = (a, b) => [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
const mul = (a, s) => [a[0] * s, a[1] * s, a[2] * s]
const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
const len = (a) => Math.sqrt(dot(a, a))
const norm = (a) => {
  const l = len(a)
  return l > 0 ? mul(a, 1 / l) : [0, 0, 0]
}
const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]

export function falloff(d, r) {
  if (d >= r) return 0
  const t = 1 - d / r
  return t * t * (3 - 2 * t)
}

function vertexNormals(pos, faces) {
  const n = pos.map(() => [0, 0, 0])
  for (const f of faces) {
    let face = [0, 0, 0]
    for (let k = 0; k < f.length; k++) face = add(face, cross(pos[f[k]], pos[f[(k + 1) % f.length]]))
    for (const i of f) n[i] = add(n[i], face)
  }
  return n.map(norm)
}

function neighbours(count, faces) {
  const out = Array.from({ length: count }, () => [])
  for (const f of faces) {
    for (let k = 0; k < f.length; k++) {
      const a = f[k]
      const b = f[(k + 1) % f.length]
      if (!out[a].includes(b)) out[a].push(b)
      if (!out[b].includes(a)) out[b].push(a)
    }
  }
  return out
}

const dist2 = (a, b) => {
  const d = sub(a, b)
  return dot(d, d)
}

function triangulate(f, pos) {
  if (f.length === 4) {
    return dist2(pos[f[0]], pos[f[2]]) <= dist2(pos[f[1]], pos[f[3]])
      ? [
          [f[0], f[1], f[2]],
          [f[0], f[2], f[3]],
        ]
      : [
          [f[0], f[1], f[3]],
          [f[1], f[2], f[3]],
        ]
  }
  const out = []
  for (let k = 1; k < f.length - 1; k++) out.push([f[0], f[k], f[k + 1]])
  return out
}

/**
 * Dynamic topology for one dab (src/sculpt.rs `refine`): polygons under the
 * brush become triangles, then edges there longer than 4/3 `detail` are
 * split at their midpoints, longest first. Mutates `pos` and `faces`.
 */
export function refine(pos, faces, c, radius, detail) {
  const reach = (radius + detail) ** 2
  const near = (p) => dist2(p, c) < reach
  let changed = false
  const extra = []
  for (let i = 0; i < faces.length; i++) {
    const f = faces[i]
    if (f.length > 3 && f.some((v) => near(pos[v]))) {
      const tris = triangulate(f, pos)
      faces[i] = tris[0]
      for (let k = 1; k < tris.length; k++) extra.push(tris[k])
      changed = true
    }
  }
  faces.push(...extra)
  const limit = detail * SPLIT_RATIO
  for (let pass = 0; pass < REFINE_PASSES; pass++) {
    if (faces.length >= MAX_FACES) break
    let long = []
    for (const f of faces) {
      if (f.length !== 3) continue
      for (let k = 0; k < 3; k++) {
        const a = f[k]
        const b = f[(k + 1) % 3]
        const l = Math.sqrt(dist2(pos[a], pos[b]))
        if (l > limit && near(mul(add(pos[a], pos[b]), 0.5))) long.push([l, Math.min(a, b), Math.max(a, b)])
      }
    }
    if (!long.length) break
    long.sort((x, y) => y[0] - x[0] || x[1] - y[1] || x[2] - y[2])
    long = long.filter((e, i) => i === 0 || e[1] !== long[i - 1][1] || e[2] !== long[i - 1][2])
    const ends = new Uint8Array(pos.length)
    for (const [, a, b] of long) ends[a] = ends[b] = 1
    const owners = new Map()
    faces.forEach((f, fi) => {
      if (!f.some((v) => ends[v])) return
      for (let k = 0; k < f.length; k++) {
        const a = f[k]
        const b = f[(k + 1) % f.length]
        const key = Math.min(a, b) * 4294967296 + Math.max(a, b)
        let list = owners.get(key)
        if (!list) owners.set(key, (list = []))
        list.push(fi)
      }
    })
    const busy = new Uint8Array(faces.length)
    const added = []
    for (const [, a, b] of long) {
      const owned = owners.get(a * 4294967296 + b)
      if (owned.some((f) => busy[f]) || faces.length + added.length + owned.length > MAX_FACES) continue
      const m = pos.length
      pos.push(mul(add(pos[a], pos[b]), 0.5))
      for (const fi of owned) {
        busy[fi] = 1
        const f = faces[fi]
        const n = f.length
        let k = 0
        while (!((f[k] === a && f[(k + 1) % n] === b) || (f[k] === b && f[(k + 1) % n] === a))) k++
        f.splice(k + 1, 0, m)
        if (n === 3) {
          const at = (i) => f[(k + i) % 4]
          faces[fi] = [at(0), at(1), at(3)]
          added.push([at(1), at(2), at(3)])
        }
      }
      changed = true
    }
    faces.push(...added)
  }
  return changed
}

const AXIS = { x: 0, y: 1, z: 2 }
function mirror(v, axis) {
  if (!axis) return null
  const m = v.slice()
  m[AXIS[axis]] = -m[AXIS[axis]]
  return m
}

/**
 * An in-progress stroke on `mesh` (object space). `add(point)` extends the
 * path and applies the new dabs; `grab(offset)` re-poses a grab from the start.
 */
export function createStroke(mesh, { brush, radius, strength, invert = false, symmetry = null, detail = null }) {
  // Dynamic topology changes the faces, so the stroke works on its own copy.
  const dyn = detail && brush !== 'grab' ? detail : null
  const faces = mesh.faces.map((f) => f.slice())
  let links = brush === 'smooth' ? neighbours(mesh.vertices.length, faces) : null
  const start = mesh.vertices.map((v) => v.slice())
  let pos = start.map((v) => v.slice())
  const points = []
  let carry = 0
  const sign = invert ? -1 : 1

  function dab(c, offset) {
    if (dyn && refine(pos, faces, c, radius, dyn) && brush === 'smooth') links = neighbours(pos.length, faces)
    const hit = []
    for (let i = 0; i < pos.length; i++) {
      const w = falloff(len(sub(pos[i], c)), radius)
      if (w > 0) hit.push([i, w])
    }
    if (!hit.length) return
    const moves = []
    if (brush === 'draw' || brush === 'inflate' || brush === 'flatten') {
      const n = vertexNormals(pos, faces)
      let area = [0, 0, 0]
      for (const [i, w] of hit) area = add(area, mul(n[i], w))
      area = norm(area)
      const height = sign * strength * radius * DAB_HEIGHT
      if (brush === 'draw') for (const [i, w] of hit) moves.push([i, mul(area, height * w)])
      else if (brush === 'inflate') for (const [i, w] of hit) moves.push([i, mul(n[i], height * w)])
      else {
        let total = 0
        let centre = [0, 0, 0]
        for (const [i, w] of hit) {
          total += w
          centre = add(centre, mul(pos[i], w))
        }
        centre = mul(centre, 1 / total)
        for (const [i, w] of hit) moves.push([i, mul(area, -dot(sub(pos[i], centre), area) * strength * w * 0.5)])
      }
    } else if (brush === 'smooth') {
      for (const [i, w] of hit) {
        if (!links[i].length) continue
        let avg = [0, 0, 0]
        for (const j of links[i]) avg = add(avg, pos[j])
        avg = mul(avg, 1 / links[i].length)
        moves.push([i, mul(sub(avg, pos[i]), strength * w)])
      }
    } else if (brush === 'grab') {
      for (const [i, w] of hit) moves.push([i, mul(offset, w)])
    }
    for (const [i, d] of moves) pos[i] = add(pos[i], d)
  }

  function both(c, offset = [0, 0, 0]) {
    dab(c, offset)
    const m = mirror(c, symmetry)
    if (m) dab(m, mirror(offset, symmetry))
  }

  return {
    points,
    add(p) {
      if (brush === 'grab') {
        if (!points.length) points.push(p)
        return
      }
      if (!points.length) {
        points.push(p)
        return both(p)
      }
      const a = points.at(-1)
      const spacing = radius * SPACING
      const l = len(sub(p, a))
      let t = spacing - carry
      while (t <= l) {
        both(add(a, mul(sub(p, a), t / l)))
        t += spacing
      }
      carry = l - (t - spacing)
      points.push(p)
    },
    grab(offset) {
      pos = start.map((v) => v.slice())
      if (points.length) both(points[0], offset)
    },
    vertices: () => pos,
    faces: () => faces,
  }
}
