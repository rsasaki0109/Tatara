// Rigs in the viewport: bone poses and skinning, mirroring src/rig.rs. The
// weights come from the core with each rigged object (`skin`), so the
// viewport bends meshes exactly like renders and exports do.

import * as THREE from 'three'
import { sampleTrack } from './anim.js'

export const hasRig = (o) => Boolean(o.bones?.length)

/** The bone keyed by a `bone` track, or null. */
export const boneTrack = (o, name) => (o.tracks || []).find((t) => t.property === 'bone' && t.bone === name && t.keys.length) || null

/** Each bone's rotation at `frame`: keyed bones follow their tracks. */
export function boneRotations(o, frame) {
  return o.bones.map((b) => {
    const t = boneTrack(o, b.name)
    return t ? sampleTrack(t, frame) : b.rotation || [0, 0, 0]
  })
}

/** Each bone's posed transform (object space, rest to posed). */
export function boneMatrices(bones, rotations) {
  const out = []
  bones.forEach((b, i) => {
    const [x, y, z] = b.head
    const r = rotations[i]
    const local = new THREE.Matrix4()
      .makeTranslation(x, y, z)
      .multiply(new THREE.Matrix4().makeRotationFromEuler(new THREE.Euler(r[0], r[1], r[2], 'XYZ')))
      .multiply(new THREE.Matrix4().makeTranslation(-x, -y, -z))
    const p = b.parent ? bones.findIndex((o) => o.name === b.parent) : -1
    out.push(p >= 0 ? out[p].clone().multiply(local) : local)
  })
  return out
}

const turned = (rotations) => rotations.some((r) => r.some((v) => Math.abs(v) > 1e-12))

/** `mesh` (the displayed one) bent by the bones at `frame`; unchanged at rest. */
export function posedMesh(o, mesh, frame) {
  if (!hasRig(o) || !o.skin || o.skin.weights.length !== mesh.vertices.length * 4) return mesh
  const rotations = boneRotations(o, frame)
  if (!turned(rotations)) return mesh
  const m = boneMatrices(o.bones, rotations).map((x) => x.elements)
  const { joints, weights } = o.skin
  const vertices = mesh.vertices.map((v, i) => {
    let [x, y, z, total] = [0, 0, 0, 0]
    for (let k = 0; k < 4; k++) {
      const w = weights[i * 4 + k]
      if (!w) continue
      const e = m[joints[i * 4 + k]]
      x += w * (e[0] * v[0] + e[4] * v[1] + e[8] * v[2] + e[12])
      y += w * (e[1] * v[0] + e[5] * v[1] + e[9] * v[2] + e[13])
      z += w * (e[2] * v[0] + e[6] * v[1] + e[10] * v[2] + e[14])
      total += w
    }
    return total > 0 ? [x / total, y / total, z / total] : v
  })
  return { ...mesh, vertices }
}

/** Posed `[head, tail]` of every bone, object space. */
export function boneSegments(o, frame) {
  const m = boneMatrices(o.bones, boneRotations(o, frame))
  return o.bones.map((b, i) => [new THREE.Vector3(...b.head).applyMatrix4(m[i]), new THREE.Vector3(...b.tail).applyMatrix4(m[i])])
}
