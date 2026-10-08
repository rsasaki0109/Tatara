// Brush strokes for the live preview while dragging. Mirrors src/sculpt.rs;
// when the stroke ends the same points go to the engine as one `sculpt`
// command, and its result replaces the preview.

export const SPACING = 0.2
const DAB_HEIGHT = 0.15

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

function neighbours(mesh) {
  const out = mesh.vertices.map(() => [])
  for (const f of mesh.faces) {
    for (let k = 0; k < f.length; k++) {
      const a = f[k]
      const b = f[(k + 1) % f.length]
      if (!out[a].includes(b)) out[a].push(b)
      if (!out[b].includes(a)) out[b].push(a)
    }
  }
  return out
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
export function createStroke(mesh, { brush, radius, strength, invert = false, symmetry = null }) {
  const faces = mesh.faces
  const links = brush === 'smooth' ? neighbours(mesh) : null
  const start = mesh.vertices.map((v) => v.slice())
  let pos = start.map((v) => v.slice())
  const points = []
  let carry = 0
  const sign = invert ? -1 : 1

  function dab(c, offset) {
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
  }
}
