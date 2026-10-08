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

/** The most one bone turns in one reach iteration (radians), as in rig.rs. */
const REACH_STEP = 0.08

const parentIndex = (bones, k) => (bones[k].parent ? bones.findIndex((b) => b.name === bones[k].parent) : -1)

/**
 * Inverse kinematics, mirroring `rig::reach`: turn bone `end` and up to
 * `chain - 1` ancestors so its tip reaches `target` (object space), by
 * cyclic coordinate descent in small steps. Returns new rotations.
 */
export function reach(bones, rotations, end, chain, target) {
  const rot = rotations.map((r) => r.slice())
  const joints = [end]
  while (joints.length < Math.max(1, chain)) {
    const p = parentIndex(bones, joints[joints.length - 1])
    if (p < 0) break
    joints.push(p)
  }
  const tail = new THREE.Vector3(...bones[end].tail)
  for (let i = 0; i < 400; i++) {
    if (tail.clone().applyMatrix4(boneMatrices(bones, rot)[end]).distanceTo(target) < 1e-5) break
    for (const j of joints) {
      const m = boneMatrices(bones, rot)
      const a = tail.clone().applyMatrix4(m[end])
      const pivot = new THREE.Vector3(...bones[j].head).applyMatrix4(m[j])
      a.sub(pivot)
      const b = target.clone().sub(pivot)
      if (a.length() < 1e-9 || b.length() < 1e-9) continue
      const turn = new THREE.Quaternion().setFromUnitVectors(a.normalize(), b.normalize())
      const angle = turn.angleTo(new THREE.Quaternion())
      if (angle > REACH_STEP) turn.slerpQuaternions(new THREE.Quaternion(), turn.clone(), REACH_STEP / angle)
      const p = parentIndex(bones, j)
      // The bone's frame in object space is its parents' turn times its own.
      const parent = p >= 0 ? new THREE.Quaternion().setFromRotationMatrix(m[p]) : new THREE.Quaternion()
      const own = new THREE.Quaternion().setFromEuler(new THREE.Euler(rot[j][0], rot[j][1], rot[j][2], 'XYZ'))
      const next = parent.clone().invert().multiply(turn).multiply(parent).multiply(own).normalize()
      const e = new THREE.Euler().setFromQuaternion(next, 'XYZ')
      rot[j] = [e.x, e.y, e.z]
    }
  }
  return rot
}
