import * as THREE from 'three'
import { OrbitControls } from 'three/addons/controls/OrbitControls.js'
import { TransformControls } from 'three/addons/controls/TransformControls.js'
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js'
import { toCreasedNormals } from 'three/addons/utils/BufferGeometryUtils.js'
import { EffectComposer } from 'three/addons/postprocessing/EffectComposer.js'
import { RenderPass } from 'three/addons/postprocessing/RenderPass.js'
import { UnrealBloomPass } from 'three/addons/postprocessing/UnrealBloomPass.js'
import { OutputPass } from 'three/addons/postprocessing/OutputPass.js'
import { ShaderPass } from 'three/addons/postprocessing/ShaderPass.js'
import { ease } from './clock.js'
import { isAnimated, pose } from './anim.js'
import { createStroke } from './sculpt.js'
import { uvGridCanvas } from './uveditor.js'
import { boneRotations, boneSegments, hasRig, posedMesh, reach } from './rig.js'

// KHR_lights_punctual specifies a squared angular ramp. Use the same
// production shader for all scene spots before compiling any materials.
const spotFunction = /float getSpotAttenuation\( const in float coneCosine, const in float penumbraCosine, const in float angleCosine \) \{[\s\S]*?\n\}/
if (!spotFunction.test(THREE.ShaderChunk.lights_pars_begin)) throw new Error('Unsupported Three.js spot attenuation shader')
THREE.ShaderChunk.lights_pars_begin = THREE.ShaderChunk.lights_pars_begin.replace(spotFunction, `float getSpotAttenuation( const in float coneCosine, const in float penumbraCosine, const in float angleCosine ) {
  float ramp = clamp((angleCosine - coneCosine) / max(0.001, penumbraCosine - coneCosine), 0.0, 1.0);
  return ramp * ramp;
}`)

const CREASE = THREE.MathUtils.degToRad(38)
const SELECT = 0xff8a4c
const WARN = 0xff4d5e
const DEG = Math.PI / 180

function meshKey(mesh) {
  let h = mesh.vertices.length * 31 + mesh.faces.length
  for (const v of mesh.vertices) h = (h * 1.000173 + v[0] * 3.1 + v[1] * 7.3 + v[2] * 11.7) % 1e9
  for (const f of mesh.faces) h = (h * 1.000071 + f.length + f[0]) % 1e9
  return h.toFixed(6)
}

function hasStraightCorner(vertices, f) {
  for (let k = 0; k < f.length; k++) {
    const p = vertices[f[(k + f.length - 1) % f.length]]
    const v = vertices[f[k]]
    const q = vertices[f[(k + 1) % f.length]]
    const a = [v[0] - p[0], v[1] - p[1], v[2] - p[2]]
    const b = [q[0] - v[0], q[1] - v[1], q[2] - v[2]]
    const cx = a[1] * b[2] - a[2] * b[1]
    const cy = a[2] * b[0] - a[0] * b[2]
    const cz = a[0] * b[1] - a[1] * b[0]
    if (Math.hypot(cx, cy, cz) <= 1e-9 * Math.hypot(...a) * Math.hypot(...b)) return true
  }
  return false
}

/** Face normal by Newell's method (same as the Rust core). */
function newell(vertices, f) {
  const n = [0, 0, 0]
  for (let k = 0; k < f.length; k++) {
    const a = vertices[f[k]]
    const b = vertices[f[(k + 1) % f.length]]
    n[0] += (a[1] - b[1]) * (a[2] + b[2])
    n[1] += (a[2] - b[2]) * (a[0] + b[0])
    n[2] += (a[0] - b[0]) * (a[1] + b[1])
  }
  return n
}

/** Box projection, as in src/texture.rs: texture coordinates in metres. */
function boxUv(p, n) {
  const [ax, ay, az] = n.map(Math.abs)
  if (ax >= ay && ax >= az) return n[0] >= 0 ? [-p[2], p[1]] : [p[2], p[1]]
  if (ay >= az) return n[1] >= 0 ? [p[0], -p[2]] : [p[0], p[2]]
  return n[2] >= 0 ? [p[0], p[1]] : [-p[0], p[1]]
}

const SIDES = [
  [1, 0, 0],
  [-1, 0, 0],
  [0, 1, 0],
  [0, -1, 0],
  [0, 0, 1],
  [0, 0, -1],
]

function sideOf(n) {
  const [ax, ay, az] = n.map(Math.abs)
  const [k, c] = ax >= ay && ax >= az ? [0, n[0]] : ay >= az ? [1, n[1]] : [2, n[2]]
  return k * 2 + (c < 0 ? 1 : 0)
}

/**
 * Box projection of one object (src/texture.rs BoxProjection): in metres
 * of its scaled size, or fitted so each side shows exactly one tile.
 */
function boxProjection(vertices, scale, fit) {
  let ranges = null
  if (fit) {
    const lo = [Infinity, Infinity, Infinity]
    const hi = [-Infinity, -Infinity, -Infinity]
    for (const v of vertices) for (let k = 0; k < 3; k++) (lo[k] = Math.min(lo[k], v[k] * scale[k])), (hi[k] = Math.max(hi[k], v[k] * scale[k]))
    ranges = SIDES.map((n) => {
      const a = [Infinity, Infinity]
      const b = [-Infinity, -Infinity]
      for (let c = 0; c < 8; c++) {
        const uv = boxUv([c & 1 ? hi[0] : lo[0], c & 2 ? hi[1] : lo[1], c & 4 ? hi[2] : lo[2]], n)
        for (let i = 0; i < 2; i++) (a[i] = Math.min(a[i], uv[i])), (b[i] = Math.max(b[i], uv[i]))
      }
      return [a[0], a[1], Math.max(b[0] - a[0], 1e-9), Math.max(b[1] - a[1], 1e-9)]
    })
  }
  const safe = (x) => (Math.abs(x) < 1e-9 ? 1e-9 : x)
  return (p, n) => {
    const ns = [n[0] / safe(scale[0]), n[1] / safe(scale[1]), n[2] / safe(scale[2])]
    const uv = boxUv([p[0] * scale[0], p[1] * scale[1], p[2] * scale[2]], ns)
    if (!ranges) return uv
    const [u0, v0, du, dv] = ranges[sideOf(ns)]
    return [(uv[0] - u0) / du, (uv[1] - v0) / dv]
  }
}

/**
 * Triplanar texturing, as in src/texture.rs `triplanar`: a box-projected
 * texture blends three axis planes by the surface normal (in the object's
 * space as scaled, so tiles stay in metres), which removes the seams box
 * projection leaves on curved surfaces. Flat axis-aligned faces get exactly
 * one plane, so boxes look as before. Enabled per material by `TRIPLANAR`.
 */
function installTriplanar(m) {
  const tp = { tpScale: { value: new THREE.Vector3(1, 1, 1) }, tpRepeat: { value: 2 } }
  m.userData.tp = tp
  m.defines = m.defines || {}
  m.customProgramCacheKey = () => 'tatara-triplanar'
  m.onBeforeCompile = (shader) => {
    Object.assign(shader.uniforms, tp)
    shader.vertexShader = shader.vertexShader
      .replace(
        '#include <common>',
        `#include <common>
#ifdef TRIPLANAR
uniform vec3 tpScale;
varying vec3 vTpPos;
varying vec3 vTpNormal;
varying vec3 vTpX;
varying vec3 vTpY;
varying vec3 vTpZ;
#endif`,
      )
      .replace(
        '#include <begin_vertex>',
        `#include <begin_vertex>
#ifdef TRIPLANAR
vTpPos = transformed * tpScale;
vTpNormal = objectNormal / tpScale;
// The object's axes (as scaled) in view space: normalMatrix undoes the scale.
vTpX = normalMatrix * vec3(tpScale.x, 0.0, 0.0);
vTpY = normalMatrix * vec3(0.0, tpScale.y, 0.0);
vTpZ = normalMatrix * vec3(0.0, 0.0, tpScale.z);
#endif`,
      )
    shader.fragmentShader = shader.fragmentShader
      .replace(
        '#include <common>',
        `#include <common>
#ifdef TRIPLANAR
uniform float tpRepeat;
varying vec3 vTpPos;
varying vec3 vTpNormal;
varying vec3 vTpX;
varying vec3 vTpY;
varying vec3 vTpZ;
struct TpPlanes { vec3 w; vec3 s; vec2 x; vec2 y; vec2 z; };
TpPlanes tpPlanes() {
  vec3 n = normalize(vTpNormal);
  vec3 a = max(abs(n) - 0.3, 0.0);
  a = a * a * a;
  TpPlanes t;
  t.w = a / max(a.x + a.y + a.z, 1e-12);
  t.s = vec3(n.x >= 0.0 ? 1.0 : -1.0, n.y >= 0.0 ? 1.0 : -1.0, n.z >= 0.0 ? 1.0 : -1.0);
  vec3 p = vTpPos;
  t.x = vec2(-t.s.x * p.z, p.y) * tpRepeat;
  t.y = vec2(p.x, -t.s.y * p.z) * tpRepeat;
  t.z = vec2(t.s.z * p.x, p.y) * tpRepeat;
  return t;
}
#endif`,
      )
      .replace(
        '#include <map_fragment>',
        `#if defined( TRIPLANAR ) && defined( USE_MAP )
{
  TpPlanes tq = tpPlanes();
  diffuseColor *= tq.w.x * texture2D(map, tq.x) + tq.w.y * texture2D(map, tq.y) + tq.w.z * texture2D(map, tq.z);
}
#else
#include <map_fragment>
#endif`,
      )
      .replace(
        '#include <roughnessmap_fragment>',
        `#if defined( TRIPLANAR ) && defined( USE_ROUGHNESSMAP )
float roughnessFactor = roughness;
{
  TpPlanes tq = tpPlanes();
  roughnessFactor *= tq.w.x * texture2D(roughnessMap, tq.x).g + tq.w.y * texture2D(roughnessMap, tq.y).g + tq.w.z * texture2D(roughnessMap, tq.z).g;
}
#else
#include <roughnessmap_fragment>
#endif`,
      )
      .replace(
        '#include <metalnessmap_fragment>',
        `#if defined( TRIPLANAR ) && defined( USE_METALNESSMAP )
float metalnessFactor = metalness;
{
  TpPlanes tq = tpPlanes();
  metalnessFactor *= tq.w.x * texture2D(metalnessMap, tq.x).b + tq.w.y * texture2D(metalnessMap, tq.y).b + tq.w.z * texture2D(metalnessMap, tq.z).b;
}
#else
#include <metalnessmap_fragment>
#endif`,
      )
      .replace(
        '#include <normal_fragment_maps>',
        `#if defined( TRIPLANAR ) && defined( USE_NORMALMAP_TANGENTSPACE )
{
  TpPlanes tq = tpPlanes();
  vec3 nx = texture2D(normalMap, tq.x).xyz * 2.0 - 1.0;
  vec3 ny = texture2D(normalMap, tq.y).xyz * 2.0 - 1.0;
  vec3 nz = texture2D(normalMap, tq.z).xyz * 2.0 - 1.0;
  vec3 d = tq.w.x * (vec3(0.0, 0.0, -tq.s.x) * nx.x + vec3(0.0, 1.0, 0.0) * nx.y)
    + tq.w.y * (vec3(1.0, 0.0, 0.0) * ny.x + vec3(0.0, 0.0, -tq.s.y) * ny.y)
    + tq.w.z * (vec3(tq.s.z, 0.0, 0.0) * nz.x + vec3(0.0, 1.0, 0.0) * nz.y);
  normal = normalize(normal + vTpX * d.x + vTpY * d.y + vTpZ * d.z);
}
#else
#include <normal_fragment_maps>
#endif`,
      )
  }
}

/** A fingerprint of a mesh's own UVs (empty when it has none). */
function uvKey(mesh) {
  if (!mesh.uvs?.length) return ''
  let h = mesh.uvs.length
  for (const f of mesh.uvs) for (const [u, v] of f) h = (h * 1.000193 + u * 5.3 + v * 9.1) % 1e9
  return `:uv${h.toFixed(6)}`
}

/** What the displayed geometry of an object depends on. */
function geometryKey(o, mesh) {
  const t = o.material?.texture
  return meshKey(mesh) + uvKey(mesh) + (o.smooth ? ':smooth' : '') + (t ? `:${o.transform.scale.join(',')}:${t.fit ? 'fit' : ''}` : '')
}

/** A short, stable fingerprint of a string (cache-busting URLs). */
function hashString(text) {
  let h = 2166136261
  for (let i = 0; i < text.length; i++) h = Math.imul(h ^ text.charCodeAt(i), 16777619)
  return (h >>> 0).toString(16)
}

/** Whether a mesh carries its own texture coordinates, one per face corner. */
export const hasUvs = (mesh) => Boolean(mesh.uvs?.length) && mesh.uvs.length === mesh.faces.length && mesh.uvs.every((u, i) => u.length === mesh.faces[i].length)

function buildGeometry(vertices, faces, smooth = false, faceUvs = null, projection = { scale: [1, 1, 1], fit: false }) {
  const pos = []
  const uv = []
  const triFace = []
  const own = faceUvs && hasUvs({ faces, uvs: faceUvs })
  const project = own ? null : boxProjection(vertices, projection.scale, projection.fit)
  faces.forEach((f, fi) => {
    const start = pos.length
    const corners = emit(f, fi)
    if (own) {
      // Corners as emitted: the fan's centre is the average of the corners.
      const fu = faceUvs[fi]
      const c = [0, 1].map((k) => fu.reduce((sum, x) => sum + x[k], 0) / fu.length)
      for (const k of corners) uv.push(...(k < 0 ? c : fu[k]))
    } else {
      const n = newell(vertices, f)
      for (let i = start; i < pos.length; i += 3) uv.push(...project([pos[i], pos[i + 1], pos[i + 2]], n))
    }
  })
  // Pushes the face's triangles; returns the corner index of each emitted
  // vertex (-1 for a centre point).
  function emit(f, fi) {
    const corners = []
    if (f.length > 3 && hasStraightCorner(vertices, f)) {
      // A corner on a straight edge (a boolean's welded seam) would give
      // a fan zero-area triangles and leave the corner out of the mesh, so
      // fan from the centre instead: every corner stays a vertex.
      const c = [0, 1, 2].map((k) => f.reduce((sum, i) => sum + vertices[i][k], 0) / f.length)
      for (let k = 0; k < f.length; k++) {
        const a = vertices[f[k]]
        const b = vertices[f[(k + 1) % f.length]]
        pos.push(c[0], c[1], c[2], a[0], a[1], a[2], b[0], b[1], b[2])
        corners.push(-1, k, (k + 1) % f.length)
        triFace.push(fi)
      }
      return corners
    }
    const a = vertices[f[0]]
    for (let k = 1; k < f.length - 1; k++) {
      const b = vertices[f[k]]
      const c = vertices[f[k + 1]]
      pos.push(a[0], a[1], a[2], b[0], b[1], b[2], c[0], c[1], c[2])
      corners.push(0, k, k + 1)
      triFace.push(fi)
    }
    return corners
  }
  const g = new THREE.BufferGeometry()
  g.setAttribute('position', new THREE.Float32BufferAttribute(pos, 3))
  g.setAttribute('uv', new THREE.Float32BufferAttribute(uv, 2))
  // Smooth shading blends normals across every edge (crease angle π).
  const geometry = toCreasedNormals(g, smooth ? Math.PI : CREASE)
  g.dispose()
  geometry.computeBoundingSphere()
  return { geometry, triFace }
}

/** The mesh an object displays: evaluated through its modifier stack, if any. */
export const displayMesh = (o) => o.display || o.mesh

function polygonEdges(vertices, faces) {
  const seen = new Set()
  const pos = []
  for (const f of faces) {
    for (let k = 0; k < f.length; k++) {
      const a = f[k]
      const b = f[(k + 1) % f.length]
      const key = a < b ? `${a},${b}` : `${b},${a}`
      if (seen.has(key)) continue
      seen.add(key)
      pos.push(...vertices[a], ...vertices[b])
    }
  }
  const g = new THREE.BufferGeometry()
  g.setAttribute('position', new THREE.Float32BufferAttribute(pos, 3))
  return g
}

function segmentDistance(p, a, b) {
  const dx = b.x - a.x
  const dy = b.y - a.y
  const len = dx * dx + dy * dy
  const t = len ? Math.max(0, Math.min(1, ((p.x - a.x) * dx + (p.y - a.y) * dy) / len)) : 0
  return Math.hypot(p.x - (a.x + t * dx), p.y - (a.y + t * dy))
}

function facesGeometry(vertices, faces) {
  const pos = []
  for (const face of faces) {
    const a = vertices[face[0]]
    for (let k = 1; k < face.length - 1; k++) pos.push(...a, ...vertices[face[k]], ...vertices[face[k + 1]])
  }
  const g = new THREE.BufferGeometry()
  g.setAttribute('position', new THREE.Float32BufferAttribute(pos, 3))
  return g
}

export class Viewport {
  constructor(el, clock, animator, { capture, onPick, onTransform, onMoveVertices, onSculpt, onReach }) {
    this.el = el
    this.clock = clock
    this.anim = animator
    this.capture = capture
    this.onPick = onPick
    this.onTransform = onTransform
    this.onMoveVertices = onMoveVertices
    this.onSculpt = onSculpt
    this.onReach = onReach
    // Sculpt mode: brush settings, and the stroke being dragged (if any).
    this.sculpt = { active: false }
    this.stroke = null
    // Objects with inspection issues get a red outline.
    this.warned = new Set()
    // Edit mode: which components of the selected object are selected.
    this.edit = { active: false, mode: 'face', verts: [], edges: [], faces: [] }
    this.nodes = new Map()
    this.textureImages = new Map()
    this.images = {}
    this.selected = null
    this.face = null
    this.wireframe = false
    this.spinRate = 0
    // Current animation frame; animated objects are posed at it.
    this.currentFrame = 1
    this.showGizmo = true
    this.lastFrame = clock.now()

    const r = (this.renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true, preserveDrawingBuffer: capture }))
    r.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2))
    r.toneMapping = THREE.ACESFilmicToneMapping
    r.toneMappingExposure = 1.0
    r.shadowMap.enabled = true
    r.shadowMap.type = THREE.PCFShadowMap
    el.prepend(r.domElement)

    const scene = (this.scene = new THREE.Scene())
    const pmrem = (this.pmrem = new THREE.PMREMGenerator(r))
    this.studioEnv = pmrem.fromScene(new RoomEnvironment(), 0.04).texture
    scene.environment = this.studioEnv
    scene.environmentIntensity = 0.55

    const key = new THREE.DirectionalLight(0xfff1e0, 2.4)
    key.position.set(4.5, 8, 3.5)
    key.castShadow = true
    key.shadow.mapSize.set(2048, 2048)
    key.shadow.bias = -0.0004
    key.shadow.normalBias = 0.02
    key.shadow.radius = 6
    Object.assign(key.shadow.camera, { left: -7, right: 7, top: 7, bottom: -7, near: 0.5, far: 30 })
    scene.add(key)
    const rim = new THREE.DirectionalLight(0xcfe0ff, 0.9)
    rim.position.set(-5, 4, -6)
    scene.add(rim)
    this.key = key
    this.rim = rim
    // The world (see setWorld): the studio until the scene says otherwise.
    this.world = null
    this.worldMap = null

    const ground = new THREE.Mesh(new THREE.PlaneGeometry(60, 60), new THREE.ShadowMaterial({ opacity: 0.28 }))
    ground.rotation.x = -Math.PI / 2
    ground.receiveShadow = true
    scene.add(ground)
    const grid = (this.grid = new THREE.GridHelper(16, 32, 0x8a8f99, 0x5a5f69))
    grid.material.transparent = true
    grid.material.opacity = 0.16
    grid.material.depthWrite = false
    grid.position.y = 0.0005
    scene.add(grid)

    this.root = new THREE.Group()
    scene.add(this.root)

    const cam = (this.camera = new THREE.PerspectiveCamera(36, 1, 0.05, 200))
    this.perspectiveCamera = cam
    this.controls = new OrbitControls(cam, r.domElement)
    this.controls.enableDamping = !capture
    this.controls.dampingFactor = 0.12
    this.setOrbit({ azimuth: 35, elevation: 22, distance: 6.5, target: [0, 0.5, 0] })

    this.gizmo = new TransformControls(cam, r.domElement)
    this.gizmo.setSize(0.85)
    this.pivot = new THREE.Object3D()
    scene.add(this.pivot)
    // Inverse kinematics: a handle at the chosen bone's tip; dragging it
    // bends the chain live, letting go commits a `reach`.
    this.ik = null
    this.ikHandle = new THREE.Mesh(
      new THREE.SphereGeometry(0.055, 20, 12),
      new THREE.MeshBasicMaterial({ color: 0xff8a4c, depthTest: false, transparent: true, opacity: 0.95 }),
    )
    this.ikHandle.renderOrder = 8
    this.ikHandle.visible = false
    scene.add(this.ikHandle)
    this.gizmo.addEventListener('objectChange', () => {
      if (this.gizmo.object === this.pivot && this.gizmo.dragging) this.previewMove()
      if (this.gizmo.object === this.ikHandle && this.gizmo.dragging) this.dragIk(this.ikHandle.position)
    })
    this.gizmo.addEventListener('dragging-changed', (e) => {
      this.controls.enabled = !e.value
      if (this.gizmo.object === this.ikHandle) {
        if (!e.value) this.commitIk()
        return
      }
      if (this.gizmo.object === this.pivot) {
        if (e.value) this.pivotStart = this.pivot.position.clone()
        else {
          const delta = this.localDelta()
          if (delta.some((v) => Math.abs(v) > 1e-9)) this.onMoveVertices(this.selected, this.editVertices(), delta)
        }
        return
      }
      if (!e.value && this.gizmo.object) {
        const g = this.gizmo.object
        this.onTransform(g.userData.id, {
          translation: g.position.toArray(),
          rotation: [g.rotation.x, g.rotation.y, g.rotation.z],
          scale: g.scale.toArray(),
        })
      }
    })
    scene.add(this.gizmo.getHelper())

    this.raycaster = new THREE.Raycaster()
    this.brushRing = new THREE.Mesh(
      new THREE.RingGeometry(0.93, 1, 64),
      new THREE.MeshBasicMaterial({ color: 0xffffff, transparent: true, opacity: 0.85, depthTest: false, side: THREE.DoubleSide }),
    )
    this.brushRing.renderOrder = 6
    this.brushRing.visible = false
    scene.add(this.brushRing)
    // Capture phase, so a stroke can switch orbiting off before OrbitControls sees the press.
    r.domElement.addEventListener(
      'pointerdown',
      (e) => {
        if (e.button === 0 && this.sculptDown(e.clientX, e.clientY, e)) r.domElement.setPointerCapture(e.pointerId)
      },
      { capture: true },
    )
    r.domElement.addEventListener('pointermove', (e) => this.sculptMove(e.clientX, e.clientY))
    r.domElement.addEventListener('pointerleave', () => !this.stroke && (this.brushRing.visible = false))
    window.addEventListener('pointerup', () => this.sculptUp())
    let down = null
    r.domElement.addEventListener('pointerdown', (e) => {
      // A detached gizmo can keep a stale hovered axis.
      down = this.gizmo.object && this.gizmo.axis ? null : { x: e.clientX, y: e.clientY }
    })
    r.domElement.addEventListener('pointerup', (e) => {
      if (!down || e.button !== 0) return
      const moved = Math.hypot(e.clientX - down.x, e.clientY - down.y)
      down = null
      if (moved > 4 || this.sculpt.active) return
      if (this.edit.active && this.selected != null) {
        this.onPick({ component: this.pickComponent(e.clientX, e.clientY, this.edit.mode) }, e)
        return
      }
      const hit = this.pick(e.clientX, e.clientY)
      this.onPick(hit, e)
    })

    new ResizeObserver(() => this.resize()).observe(el)
    this.resize()
  }

  resize() {
    const w = this.el.clientWidth
    const h = this.el.clientHeight
    if (!w || !h) return
    this.renderer.setSize(w, h, false)
    this.composer?.setSize(w, h)
    this.glowComposer?.setSize(w, h)
    this.renderer.domElement.style.width = `${w}px`
    this.renderer.domElement.style.height = `${h}px`
    this.camera.aspect = w / h
    if (this.camera.isOrthographicCamera) this.setOrthoExtent(this.orthoHeight)
    else this.camera.updateProjectionMatrix()
  }

  // -- scene sync -----------------------------------------------------------

  sync(scene, animate, preview = false) {
    this.framePreview = preview ? scene : null
    this.frameEpoch = (this.frameEpoch || 0) + 1
    this.frameRevision = scene.revision
    this.frameConstrained = !!(scene.constraints?.length || scene.arrangements?.length || scene.ik_targets?.length)
    this.framePoses = null
    this.frameRequested = null
    this.images = scene.images || {}
    this.setWorld(scene.world)
    const seen = new Set()
    for (const o of scene.objects) {
      seen.add(o.id)
      const node = this.nodes.get(o.id)
      if (node) this.update(node, o, animate)
      else this.create(o, animate)
    }
    for (const [id, node] of this.nodes) {
      if (seen.has(id)) continue
      this.nodes.delete(id)
      if (node.light) { this.scene.remove(node.light, node.light.target); node.light.dispose() }
      if (this.gizmo.object === node.group) this.gizmo.detach()
      const done = () => {
        this.root.remove(node.group)
        node.mesh.geometry.dispose()
        node.mesh.material.map?.dispose()
        node.glowMat?.dispose()
      }
      if (animate) {
        const s = node.group.scale.clone()
        this.anim.add(`tf:${id}`, 260, (t) => node.group.scale.copy(s).multiplyScalar(Math.max(0.0001, 1 - t)), ease.inOut).then(done)
      } else done()
    }
    this.setSelection(this.selected, this.face)
    this.applyWireframe()
    this.setFrame(this.currentFrame)
  }

  create(o, animate) {
    const group = new THREE.Group()
    group.userData.id = o.id
    const material = new THREE.MeshPhysicalMaterial({ clearcoatRoughness: 0.18 })
    installTriplanar(material)
    const mesh = new THREE.Mesh(new THREE.BufferGeometry(), material)
    mesh.castShadow = !o.camera && !o.light
    mesh.receiveShadow = !o.camera && !o.light
    mesh.userData.id = o.id
    material.userData.objectId = o.id
    const lineMat = new THREE.LineBasicMaterial({ color: 0x15171a, transparent: true, opacity: 0.55 })
    const wire = new THREE.LineSegments(new THREE.BufferGeometry(), lineMat)
    const outline = new THREE.LineSegments(
      new THREE.BufferGeometry(),
      new THREE.LineBasicMaterial({ color: SELECT, transparent: true, opacity: 0.95 }),
    )
    const faceMark = new THREE.Mesh(
      new THREE.BufferGeometry(),
      new THREE.MeshBasicMaterial({
        color: SELECT,
        transparent: true,
        opacity: 0.55,
        side: THREE.DoubleSide,
        polygonOffset: true,
        polygonOffsetFactor: -2,
        polygonOffsetUnits: -2,
        depthWrite: false,
      }),
    )
    // The cage is the editable base mesh: invisible for picking faces, drawn
    // as orange lines when a modifier stack changes what is displayed.
    const cage = new THREE.Mesh(new THREE.BufferGeometry(), new THREE.MeshBasicMaterial({ visible: false }))
    cage.userData.id = o.id
    const cageLines = new THREE.LineSegments(
      new THREE.BufferGeometry(),
      new THREE.LineBasicMaterial({ color: SELECT, transparent: true, opacity: 0.7, depthTest: false }),
    )
    const points = new THREE.Points(
      new THREE.BufferGeometry(),
      new THREE.PointsMaterial({ color: 0x15171a, size: 5, sizeAttenuation: false, depthTest: false }),
    )
    const selPoints = new THREE.Points(
      new THREE.BufferGeometry(),
      new THREE.PointsMaterial({ color: SELECT, size: 8, sizeAttenuation: false, depthTest: false }),
    )
    const selEdges = new THREE.LineSegments(
      new THREE.BufferGeometry(),
      new THREE.LineBasicMaterial({ color: 0xffd0a8, depthTest: false }),
    )
    // UV seams, drawn red like Blender's while editing or unwrapping.
    const seams = new THREE.LineSegments(new THREE.BufferGeometry(), new THREE.LineBasicMaterial({ color: 0xff3b3b, depthTest: false }))
    seams.visible = false
    seams.renderOrder = 4
    for (const x of [wire, outline, faceMark, cageLines, points, selPoints, selEdges]) {
      x.visible = false
      x.renderOrder = 2
    }
    cageLines.renderOrder = 3
    points.renderOrder = 4
    selEdges.renderOrder = 4
    selPoints.renderOrder = 5
    // Bones, drawn in front like Blender's: octahedral shapes with edges.
    const bones = new THREE.Mesh(
      new THREE.BufferGeometry(),
      new THREE.MeshBasicMaterial({ vertexColors: true, transparent: true, opacity: 0.85, depthTest: false, side: THREE.DoubleSide }),
    )
    const boneEdges = new THREE.LineSegments(new THREE.BufferGeometry(), new THREE.LineBasicMaterial({ color: 0x14161a, depthTest: false }))
    bones.visible = boneEdges.visible = false
    bones.renderOrder = 6
    boneEdges.renderOrder = 7
    group.add(mesh, wire, outline, faceMark, cage, cageLines, points, selPoints, selEdges, seams, bones, boneEdges)
    this.root.add(group)
    const node = { id: o.id, group, mesh, wire, outline, faceMark, cage, cageLines, points, selPoints, selEdges, seams, bones, boneEdges, data: o, key: null, cageKey: null, triFace: [] }
    this.nodes.set(o.id, node)
    const posed = pose(o, this.currentFrame)
    this.setTransform(node.group, posed.transform)
    this.setMaterial(node, posed.material)
    this.setMesh(node, this.shownMesh(o))
    this.setCage(node, o.mesh)
    this.setBones(node)
    if (animate) {
      const target = node.group.scale.clone()
      this.anim.add(`tf:${o.id}`, 520, (t) => node.group.scale.copy(target).multiplyScalar(Math.max(0.0001, t)), ease.back)
    }
  }

  update(node, o, animate) {
    const prev = node.data
    node.data = o
    // Rigged meshes show their pose; they swap in without a morph.
    const plain = !o.display && !prev.display && !hasRig(o) && !hasRig(prev)
    const shown = this.shownMesh(o)
    const key = geometryKey(o, shown)
    // A committed stroke is already on screen: swap in the result, no morph.
    const sculpted = node.sculptPreview
    node.sculptPreview = false
    if (key !== node.key) {
      const morph = animate && plain && !sculpted && (this.morphStart(prev.mesh, o.mesh) || this.sameTopology(prev.mesh, o.mesh))
      if (morph) {
        const end = o.mesh.vertices
        const verts = end.map((v) => v.slice())
        this.anim.add(`mesh:${o.id}`, 480, (t) => {
          for (let i = 0; i < end.length; i++) {
            const s = morph[i]
            for (let k = 0; k < 3; k++) verts[i][k] = s[k] + (end[i][k] - s[k]) * t
          }
          const live = { vertices: verts, faces: o.mesh.faces, uvs: o.mesh.uvs }
          this.setMesh(node, live, t < 1 ? null : key)
          this.setCage(node, live, t < 1 ? null : meshKey(o.mesh))
        })
      } else {
        this.setMesh(node, shown, key)
        if (animate && !(hasRig(o) && hasRig(prev))) this.flash(node)
      }
    }
    this.setBones(node)
    if (meshKey(o.mesh) !== node.cageKey && !this.anim.has(`mesh:${o.id}`)) this.setCage(node, o.mesh)
    else if (JSON.stringify(o.mesh.seams || []) !== node.seamKey) this.setSeams(node, o.mesh)
    const posed = pose(o, this.currentFrame)
    const tf = posed.transform
    const animated = isAnimated(o)
    const g = node.group
    const same =
      g.position.distanceTo(new THREE.Vector3(...tf.translation)) < 1e-6 &&
      g.scale.distanceTo(new THREE.Vector3(...tf.scale)) < 1e-6 &&
      new THREE.Vector3(g.rotation.x, g.rotation.y, g.rotation.z).distanceTo(new THREE.Vector3(...tf.rotation)) < 1e-6
    if (!same && animate && !animated) {
      const p0 = g.position.clone()
      const s0 = g.scale.clone()
      const q0 = g.quaternion.clone()
      const p1 = new THREE.Vector3(...tf.translation)
      const s1 = new THREE.Vector3(...tf.scale)
      const q1 = new THREE.Quaternion().setFromEuler(new THREE.Euler(...tf.rotation, 'XYZ'))
      this.anim.add(`tf:${o.id}`, 480, (t) => {
        g.position.lerpVectors(p0, p1, t)
        g.scale.lerpVectors(s0, s1, t)
        g.quaternion.slerpQuaternions(q0, q1, t)
        if (t === 1) this.setTransform(g, tf)
      })
    } else if (!same) {
      this.setTransform(g, tf)
    }
    if (JSON.stringify(pose(prev, this.currentFrame).material) !== JSON.stringify(posed.material)) {
      if (animate && !animated) {
        const m = node.mesh.material
        const from = this.materialState(m)
        this.applyMaterial(m, posed.material)
        const to = this.materialState(m)
        this.anim.add(`mat:${o.id}`, 420, (t) => {
          // A textured surface's colour lives in its texture.
          if (!m.userData.bakedColor) m.color.lerpColors(from.color, to.color, t)
          m.emissive.lerpColors(from.emissive, to.emissive, t)
          for (const k of ['roughness', 'metalness', 'clearcoat', 'emissiveIntensity', 'opacity', 'transmission']) m[k] = from[k] + (to[k] - from[k]) * t
        })
      } else this.setMaterial(node, posed.material)
    }
  }

  /** Start positions when only vertex positions changed (moves, undo of moves). */
  sameTopology(before, after) {
    if (before.vertices.length !== after.vertices.length || before.faces.length !== after.faces.length) return null
    if (after.faces.length > 20000) return null
    for (let i = 0; i < after.faces.length; i++) {
      const a = before.faces[i]
      const b = after.faces[i]
      if (a.length !== b.length) return null
      for (let k = 0; k < a.length; k++) if (a[k] !== b[k]) return null
    }
    return before.vertices
  }

  /** Start positions for an extrusion-like change: new vertices grow out of their nearest old ones. */
  morphStart(before, after) {
    const n = before.vertices.length
    if (after.vertices.length <= n || after.vertices.length - n > 512 || after.faces.length > 20000) return null
    for (let i = 0; i < n; i++) {
      const a = before.vertices[i]
      const b = after.vertices[i]
      if (Math.abs(a[0] - b[0]) + Math.abs(a[1] - b[1]) + Math.abs(a[2] - b[2]) > 1e-9) return null
    }
    // The base of each new vertex is the old vertex it shares a side edge with.
    const start = after.vertices.map((v) => v)
    for (const f of after.faces) {
      for (let k = 0; k < f.length; k++) {
        const a = f[k]
        const b = f[(k + 1) % f.length]
        if (a >= n && b < n && start[a] === after.vertices[a]) start[a] = before.vertices[b]
        if (b >= n && a < n && start[b] === after.vertices[b]) start[b] = before.vertices[a]
      }
    }
    return start
  }

  flash(node) {
    const m = node.mesh.material
    const base = m.emissive.clone()
    this.anim.add(`flash:${node.id}`, 600, (t) => {
      m.emissive.copy(base).lerp(new THREE.Color(SELECT), 0.35 * (1 - t))
    }, ease.out)
  }

  setTransform(g, tf) {
    g.position.set(...tf.translation)
    g.rotation.set(...tf.rotation, 'XYZ')
    g.scale.set(...tf.scale)
  }

  applyMaterial(m, mat) {
    if (m.userData.uvPreview) this.applyUvGrid(m)
    else this.applyTexture(m, mat)
    if (!m.userData.bakedColor || !m.map) m.color.set(mat.color)
    // A baked roughness/metalness map already holds the material's values.
    m.roughness = m.userData.ormUrl ? 1 : mat.roughness
    m.metalness = m.userData.ormUrl ? 1 : mat.metalness
    // Glass has its own reflections; a clearcoat on top only clouds it.
    m.clearcoat = mat.transmission ? 0 : Math.max(0, 0.65 - mat.roughness)
    m.emissive.set(mat.emissive || '#000000')
    m.emissiveIntensity = mat.emissive_strength ?? 1
    m.userData.glow = m.emissiveIntensity > 0 && m.emissive.r + m.emissive.g + m.emissive.b > 0.02
    m.opacity = mat.opacity ?? 1
    const transparent = m.opacity < 0.999
    if (m.transparent !== transparent) {
      m.transparent = transparent
      m.depthWrite = !transparent
      m.needsUpdate = true
    }
    m.transmission = mat.transmission || 0
    m.thickness = m.transmission ? 0.1 : 0
    m.ior = 1.5
  }

  /**
   * Textures come from the Rust core so they match the renderer and glTF
   * export: pattern tiles and relief normal maps are baked by
   * `/api/texture`, images are served as stored by `/api/image`. Geometry
   * carries box-projected UVs in metres (or the mesh's own UVs), and
   * `repeat` turns metres into tiles of `scale` metres.
   */
  applyTexture(m, mat) {
    const t = mat.texture
    const color2 = t?.color2 ?? '#3b2a22'
    const params = (extra) => new URLSearchParams({ pattern: t.pattern, color2, size: '512', ...extra }).toString()
    // A node graph is baked per object; the version busts the cache when
    // anything it depends on changes.
    const graph = t?.pattern === 'nodes' ? t.graph : null
    const nodeUrl = (kind) => {
      const v = hashString(JSON.stringify([mat.color, mat.roughness, mat.metalness, t, graph?.nodes.filter((n) => n.type === 'image').map((n) => this.images[n.image]?.hash)]))
      return `/api/nodes?${new URLSearchParams({ object: m.userData.objectId, kind, size: '512', v })}`
    }
    const colorUrl = !t || t.pattern === 'none' ? null : graph ? nodeUrl('color') : t.pattern === 'image' ? this.imageUrl(t.image) : `/api/texture?${params({ color: mat.color })}`
    const imageVersion = t?.pattern === 'image' ? { image: t.image, v: this.images[t.image]?.hash ?? '' } : {}
    const normalUrl = !t
      ? null
      : t.normal_map
        ? this.imageUrl(t.normal_map)
        : t.relief > 0 && graph
          ? graph.output?.height
            ? nodeUrl('normal')
            : null
          : t.relief > 0
            ? `/api/texture?${params({ kind: 'normal', relief: String(t.relief), ...imageVersion })}`
            : null
    // A node graph's roughness (G) and metalness (B) come as one map.
    const ormUrl = graph && (graph.output?.roughness || graph.output?.metalness) ? nodeUrl('orm') : null
    m.userData.ormUrl = ormUrl
    // A baked pattern already holds the colour; an image is tinted by it.
    m.userData.bakedColor = Boolean(colorUrl) && t.pattern !== 'image'
    m.userData.textureScale = t?.scale ?? 0.5
    m.userData.textured = Boolean(colorUrl || normalUrl)
    this.setMap(m, 'map', colorUrl)
    this.setMap(m, 'normalMap', normalUrl)
    this.setMap(m, 'roughnessMap', ormUrl)
    this.setMap(m, 'metalnessMap', ormUrl)
    this.syncRepeat(m)
  }

  imageUrl(name) {
    return `/api/image?${new URLSearchParams({ name, v: this.images[name]?.hash ?? '' })}`
  }

  syncRepeat(m) {
    const repeat = m.userData.tiled ? 1 : 1 / (m.userData.textureScale ?? 0.5)
    for (const slot of ['map', 'normalMap', 'roughnessMap', 'metalnessMap']) m[slot]?.repeat.setScalar(repeat)
    // Box-projected textures blend three planes in the shader instead.
    m.userData.tp.tpRepeat.value = 1 / (m.userData.textureScale ?? 0.5)
    const triplanar = Boolean(m.userData.textured) && !m.userData.tiled
    if (triplanar !== ('TRIPLANAR' in m.defines)) {
      if (triplanar) m.defines.TRIPLANAR = ''
      else delete m.defines.TRIPLANAR
      m.needsUpdate = true
    }
  }

  setMap(m, slot, url) {
    const key = `${slot}Url`
    if (m.userData[key] === url) return
    m.userData[key] = url
    if (!url) {
      if (m[slot]) {
        m[slot].dispose()
        m[slot] = null
        m.needsUpdate = true
      }
      return
    }
    this.loadImage(url).then((image) => {
      if (m.userData[key] !== url) return
      // Forget a map that failed to load so the next update asks again.
      if (!image) return void (m.userData[key] = undefined)
      const tex = new THREE.Texture(image)
      tex.wrapS = tex.wrapT = THREE.RepeatWrapping
      tex.colorSpace = slot === 'map' ? THREE.SRGBColorSpace : THREE.NoColorSpace
      tex.anisotropy = 8
      tex.needsUpdate = true
      m[slot]?.dispose()
      m[slot] = tex
      this.syncRepeat(m)
      if (slot === 'map' && m.userData.bakedColor) m.color.set('#ffffff')
      m.needsUpdate = true
    })
  }

  /**
   * Light the view like the scene's world (src/world.rs): the studio's
   * room environment, key and rim lights; or an environment map from the
   * core, with its sun (if it has one) as the shadow-casting key light.
   * `background` shows the world behind the scene.
   */
  setWorld(world) {
    const w = { sky: 'studio', image: null, strength: 1, rotation: 0, background: false, ...world }
    const studio = w.sky === 'studio' && !w.image
    const source = studio ? 'studio' : w.image ? `image:${w.image}:${this.images[w.image]?.hash}` : `sky:${w.sky}`
    const prev = this.world
    if (prev && prev.source === source && JSON.stringify(prev.settings) === JSON.stringify(w)) return
    this.world = w
    w.source = source
    w.settings = { ...w }
    delete w.settings.source
    const map = this.worldMap
    if (map?.source === source) return this.applyWorld()
    // Studio lighting needs no map unless its dome shows as background.
    if (studio && !w.background) {
      this.applyWorld()
      return
    }
    const ask = this.clock.track(
      fetch('/api/environment?w=512')
        .then((r) => (r.ok ? r.arrayBuffer() : Promise.reject(new Error(r.statusText))))
        .catch(() => null),
    )
    ask.then((buf) => {
      if (!buf || this.world?.source !== source) return
      const head = new DataView(buf)
      const [width, height] = [head.getUint32(0, true), head.getUint32(4, true)]
      const f = new Float32Array(buf, 8, 6)
      const texels = new Float32Array(buf, 32, width * height * 4)
      const half = new Uint16Array(texels.length)
      for (let i = 0; i < texels.length; i++) half[i] = THREE.DataUtils.toHalfFloat(texels[i])
      const tex = new THREE.DataTexture(half, width, height, THREE.RGBAFormat, THREE.HalfFloatType)
      tex.mapping = THREE.EquirectangularReflectionMapping
      tex.colorSpace = THREE.LinearSRGBColorSpace
      tex.magFilter = tex.minFilter = THREE.LinearFilter
      tex.needsUpdate = true
      this.worldMap?.texture.dispose()
      this.worldMap?.env?.dispose()
      const sun = f[3] + f[4] + f[5] > 0 ? { dir: new THREE.Vector3(f[0], f[1], f[2]), power: [f[3], f[4], f[5]] } : null
      this.worldMap = { source, texels, width, height, texture: tex, env: studio ? null : this.pmrem.fromEquirectangular(tex).texture, sun }
      this.applyWorld()
    })
    this.applyWorld()
  }

  /** Apply the current world's settings to the lights and backdrop. */
  applyWorld() {
    const w = this.world
    if (!w) return
    const scene = this.scene
    const map = this.worldMap?.source === w.source ? this.worldMap : null
    const studio = w.source === 'studio'
    // Turns match the core's: a world turned by +r is looked up at -r.
    const turn = THREE.MathUtils.degToRad(w.rotation)
    const up = new THREE.Vector3(0, 1, 0)
    if (studio) {
      scene.environment = this.studioEnv
      scene.environmentIntensity = 0.55 * w.strength
      scene.environmentRotation.set(0, 0, 0)
      this.key.color.set(0xfff1e0)
      this.key.intensity = 2.4 * w.strength
      this.key.position.set(4.5, 8, 3.5).applyAxisAngle(up, -turn)
      this.rim.intensity = 0.9 * w.strength
      this.rim.position.set(-5, 4, -6).applyAxisAngle(up, -turn)
    } else if (map) {
      scene.environment = map.env
      scene.environmentIntensity = w.strength
      scene.environmentRotation.set(0, -turn, 0)
      this.rim.intensity = 0
      if (map.sun) {
        const peak = Math.max(...map.sun.power, 1e-6)
        this.key.color.setRGB(...map.sun.power.map((v) => v / peak), THREE.LinearSRGBColorSpace)
        this.key.intensity = peak * w.strength
        this.key.position.copy(map.sun.dir).applyAxisAngle(up, -turn).multiplyScalar(10)
      } else this.key.intensity = 0
    }
    const shown = w.background && map
    scene.background = shown ? map.texture : null
    scene.backgroundIntensity = w.strength
    scene.backgroundRotation.set(0, -turn, 0)
    this.grid.visible = !shown
  }

  // Parallel rays see one environment direction across an orthographic frame.
  updateProjectionBackground() {
    const w = this.world, map = this.worldMap
    if (!w?.background || map?.source !== w.source) return
    if (!this.camera.isOrthographicCamera) { this.scene.background = map.texture; return }
    const d = this.camera.getWorldDirection(new THREE.Vector3())
      .applyAxisAngle(new THREE.Vector3(0, 1, 0), THREE.MathUtils.degToRad(w.rotation))
    const x = (Math.atan2(d.z, d.x) / (2 * Math.PI) + .5) * map.width - .5
    const y = (.5 + Math.asin(THREE.MathUtils.clamp(d.y, -1, 1)) / Math.PI) * map.height - .5
    const x0 = Math.floor(x), y0 = Math.floor(y), fx = x - x0, fy = y - y0
    const at = (i, j, c) => map.texels[(THREE.MathUtils.clamp(j, 0, map.height - 1) * map.width + ((i % map.width) + map.width) % map.width) * 4 + c]
    const rgb = [0, 1, 2].map(c => (1-fy)*((1-fx)*at(x0,y0,c)+fx*at(x0+1,y0,c)) + fy*((1-fx)*at(x0,y0+1,c)+fx*at(x0+1,y0+1,c)))
    if (!this.orthoBackground) {
      this.orthoBackground = new THREE.DataTexture(new Uint16Array(4), 1, 1, THREE.RGBAFormat, THREE.HalfFloatType)
      this.orthoBackground.colorSpace = THREE.LinearSRGBColorSpace
    }
    this.orthoBackground.image.data.set([...rgb, 1].map(v => THREE.DataUtils.toHalfFloat(v)))
    this.orthoBackground.needsUpdate = true
    this.scene.background = this.orthoBackground
  }

  loadImage(url) {
    let image = this.textureImages.get(url)
    if (!image) {
      // A busy server or decoder can fail once; try again before giving up.
      const attempt = (left) =>
        fetch(url)
          .then((r) => {
            if (!r.ok) throw new Error(r.statusText)
            return r.blob()
          })
          .then(async (blob) => {
            const img = new Image()
            img.src = URL.createObjectURL(blob)
            await img.decode()
            return img
          })
          .catch((e) => (left > 1 ? attempt(left - 1) : Promise.reject(e)))
      image = this.clock.track(
        attempt(3)
          .catch(() => {
            this.textureImages.delete(url)
            return null
          }),
      )
      this.textureImages.set(url, image)
    }
    return image
  }

  materialState(m) {
    const { roughness, metalness, clearcoat, emissiveIntensity, opacity, transmission } = m
    return { color: m.color.clone(), emissive: m.emissive.clone(), roughness, metalness, clearcoat, emissiveIntensity, opacity, transmission }
  }

  /** Whether any object emits light (and the bloom pass is needed). */
  glowing() {
    for (const node of this.nodes.values()) if (node.mesh.material.userData.glow) return true
    return false
  }

  /**
   * Selective bloom: a second render where only emitting surfaces show (the
   * rest is black so it still occludes), blurred and added over the scene.
   */
  bloom() {
    if (!this.composer) {
      const w = this.el.clientWidth
      const h = this.el.clientHeight
      const glow = (this.glowComposer = new EffectComposer(this.renderer))
      glow.renderToScreen = false
      glow.addPass(new RenderPass(this.scene, this.camera))
      const bloom = new UnrealBloomPass(new THREE.Vector2(w, h), 1.2, 0, 0)
      // Weight the blur toward the sharp mips: a tight halo, not a fog.
      bloom.compositeMaterial.uniforms.bloomFactors.value = [1, 0.5, 0.15, 0.03, 0]
      glow.addPass(bloom)
      const mix = new ShaderPass(
        new THREE.ShaderMaterial({
          uniforms: { baseTexture: { value: null }, glowTexture: { value: glow.renderTarget2.texture } },
          vertexShader: 'varying vec2 vUv; void main() { vUv = uv; gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }',
          // Alpha stays the scene's, so glow over the transparent canvas adds
          // to the page background (the canvas is premultiplied).
          fragmentShader:
            'uniform sampler2D baseTexture; uniform sampler2D glowTexture; varying vec2 vUv; void main() { vec4 b = texture2D(baseTexture, vUv); gl_FragColor = vec4(b.rgb + texture2D(glowTexture, vUv).rgb, b.a); }',
        }),
        'baseTexture',
      )
      const composer = (this.composer = new EffectComposer(this.renderer))
      composer.addPass(new RenderPass(this.scene, this.camera))
      composer.addPass(mix)
      composer.addPass(new OutputPass())
      this.black = new THREE.MeshBasicMaterial({ color: 0x000000 })
      for (const c of [glow, composer]) c.setSize(w, h)
    }
    return this.composer
  }

  renderGlow() {
    const hidden = []
    const swapped = []
    this.scene.traverseVisible((obj) => {
      const node = obj.userData.id != null && obj.isMesh && this.nodes.get(obj.userData.id)
      if (node && node.mesh === obj) {
        const m = obj.material
        let glowMat = this.black
        if (m.userData.glow) {
          glowMat = node.glowMat ||= new THREE.MeshBasicMaterial()
          glowMat.color.copy(m.emissive).multiplyScalar(m.emissiveIntensity * 0.7)
        }
        swapped.push([obj, m])
        obj.material = glowMat
      } else if (obj.isMesh || obj.isLine || obj.isPoints || obj.isSprite) {
        hidden.push(obj)
        obj.visible = false
      }
    })
    this.glowComposer.render()
    for (const [obj, m] of swapped) obj.material = m
    for (const obj of hidden) obj.visible = true
  }

  setMaterial(node, mat) {
    this.applyMaterial(node.mesh.material, mat)
  }

  setMesh(node, mesh, key = geometryKey(node.data, mesh)) {
    const o = node.data
    const fit = Boolean(o.material?.texture?.fit)
    const { geometry } = buildGeometry(mesh.vertices, mesh.faces, o.smooth, mesh.uvs, { scale: o.transform.scale, fit })
    const m = node.mesh.material
    m.userData.tp.tpScale.value.set(...o.transform.scale)
    // Own or fitted UVs are already in tiles; box projection is in metres.
    const tiled = hasUvs(mesh) || fit
    if (m.userData.tiled !== tiled) {
      m.userData.tiled = tiled
      this.syncRepeat(m)
    }
    node.mesh.geometry.dispose()
    node.mesh.geometry = geometry
    node.wire.geometry.dispose()
    node.wire.geometry = polygonEdges(mesh.vertices, mesh.faces)
    node.outline.geometry.dispose()
    node.outline.geometry = new THREE.EdgesGeometry(geometry, 32)
    if (key) node.key = key
  }

  setCage(node, base, key = meshKey(base)) {
    const { geometry, triFace } = buildGeometry(base.vertices, base.faces)
    node.cage.geometry.dispose()
    node.cage.geometry = geometry
    node.triFace = triFace
    node.cageLines.geometry.dispose()
    node.cageLines.geometry = polygonEdges(base.vertices, base.faces)
    node.points.geometry.dispose()
    node.points.geometry = new THREE.BufferGeometry().setAttribute('position', new THREE.Float32BufferAttribute(base.vertices.flat(), 3))
    node.baseMesh = base
    if (key) node.cageKey = key
    this.setSeams(node, base)
    if (this.selected === node.id) this.setSelection(node.id, this.face)
  }

  setSeams(node, base) {
    const pos = []
    for (const [a, b] of base.seams || []) pos.push(...base.vertices[a], ...base.vertices[b])
    node.seams.geometry.dispose()
    node.seams.geometry = new THREE.BufferGeometry().setAttribute('position', new THREE.Float32BufferAttribute(pos, 3))
    node.seamKey = JSON.stringify(base.seams || [])
    node.seams.visible = pos.length > 0 && ((this.edit.active && this.selected === node.id) || this.uvPreview === node.id)
  }

  /**
   * Show a UV grid on object `id` (or none) so its unwrap reads at a
   * glance, as in Blender's UV editing; its own textures come back after.
   */
  setUvPreview(id) {
    if (this.uvPreview === id) return
    const prev = this.nodes.get(this.uvPreview)
    this.uvPreview = id
    for (const node of [prev, this.nodes.get(id)]) {
      if (!node) continue
      const m = node.mesh.material
      m.userData.uvPreview = node.id === id
      if (!m.userData.uvPreview) {
        // Let the real textures load again.
        for (const slot of ['map', 'normalMap', 'roughnessMap', 'metalnessMap']) {
          m[slot] = null
          m.userData[`${slot}Url`] = undefined
        }
      }
      this.applyMaterial(m, this.objectPose(node.data).material)
      m.needsUpdate = true
      this.setSeams(node, node.baseMesh || node.data.mesh)
    }
  }

  applyUvGrid(m) {
    this.uvGrid ||= Object.assign(new THREE.CanvasTexture(uvGridCanvas()), { colorSpace: THREE.SRGBColorSpace, anisotropy: 8 })
    for (const slot of ['normalMap', 'roughnessMap', 'metalnessMap']) {
      m[slot] = null
      m.userData[`${slot}Url`] = undefined
    }
    m.map = this.uvGrid
    m.userData.mapUrl = '#uv-grid'
    m.userData.ormUrl = null
    m.userData.bakedColor = true
    m.userData.textured = false
    m.userData.textureScale = 1
    this.syncRepeat(m)
  }

  setSelection(id, face) {
    this.selected = this.nodes.has(id) ? id : null
    this.face = this.selected != null ? face : null
    const ed = this.edit
    for (const node of this.nodes.values()) {
      const on = node.id === this.selected
      const editing = on && ed.active
      const mesh = node.baseMesh
      const warned = this.warned.has(node.id)
      const mark = this.highlights?.get(node.id)
      node.outline.visible = (on && !editing) || ((warned || mark != null) && !editing)
      node.outline.material.color.set(on ? SELECT : mark ?? WARN)
      node.cageLines.visible = editing || (on && Boolean(node.data.display))
      node.seams.visible = node.seams.geometry.attributes.position?.count > 0 && (editing || this.uvPreview === node.id)
      this.setBones(node)
      node.cageLines.material.opacity = editing ? 0.9 : 0.7
      node.cageLines.material.color.set(editing ? 0x111316 : SELECT)
      node.points.visible = editing && ed.mode === 'vertex'
      const faces = (editing ? (ed.mode === 'face' ? ed.faces : []) : this.face != null ? [this.face] : []).filter((f) => on && f < mesh.faces.length)
      node.faceMark.visible = faces.length > 0
      if (faces.length) {
        node.faceMark.geometry.dispose()
        node.faceMark.geometry = facesGeometry(mesh.vertices, faces.map((f) => mesh.faces[f]))
      }
      const verts = editing && ed.mode === 'vertex' ? ed.verts.filter((v) => v < mesh.vertices.length) : []
      node.selPoints.visible = verts.length > 0
      if (verts.length) {
        node.selPoints.geometry.dispose()
        node.selPoints.geometry = new THREE.BufferGeometry().setAttribute(
          'position',
          new THREE.Float32BufferAttribute(verts.flatMap((v) => mesh.vertices[v]), 3),
        )
      }
      const edges = editing && ed.mode === 'edge' ? ed.edges.filter(([a, b]) => a < mesh.vertices.length && b < mesh.vertices.length) : []
      node.selEdges.visible = edges.length > 0
      if (edges.length) {
        node.selEdges.geometry.dispose()
        node.selEdges.geometry = new THREE.BufferGeometry().setAttribute(
          'position',
          new THREE.Float32BufferAttribute(edges.flatMap(([a, b]) => [...mesh.vertices[a], ...mesh.vertices[b]]), 3),
        )
      }
    }
    this.placeGizmo()
  }

  /** Outline objects that inspection flagged (ids), or none. */
  setWarnings(ids = []) {
    this.warned = new Set(ids)
    this.setSelection(this.selected, this.face)
  }

  /**
   * Dashed lines between related objects (constraints of the selection):
   * `[{ from: [ids], to: [ids], color }]`. They follow the objects as they
   * move and draw over everything.
   */
  setLinks(links = []) {
    if (!this.linkGroup) {
      this.linkGroup = new THREE.Group()
      this.scene.add(this.linkGroup)
    }
    for (const line of [...this.linkGroup.children]) {
      line.geometry.dispose()
      line.material.dispose()
      this.linkGroup.remove(line)
    }
    for (const link of links) {
      const g = new THREE.BufferGeometry().setAttribute('position', new THREE.Float32BufferAttribute(new Float32Array(6), 3))
      const m = new THREE.LineDashedMaterial({ color: link.color, dashSize: 0.07, gapSize: 0.05, depthTest: false, transparent: true, opacity: 0.9 })
      const line = new THREE.Line(g, m)
      line.renderOrder = 9
      line.frustumCulled = false
      line.userData.link = link
      this.linkGroup.add(line)
    }
    this.placeLinks()
  }

  placeLinks() {
    if (!this.linkGroup?.children.length) return
    const centre = (ids) => {
      const box = new THREE.Box3()
      for (const id of ids) {
        const node = this.nodes.get(id)
        if (node) box.expandByObject(node.mesh)
      }
      return box.isEmpty() ? null : box.getCenter(new THREE.Vector3())
    }
    for (const line of this.linkGroup.children) {
      const { from, to } = line.userData.link
      const a = centre(from)
      const b = centre(to)
      line.visible = Boolean(a && b)
      if (!a || !b) continue
      const p = line.geometry.attributes.position
      p.setXYZ(0, a.x, a.y, a.z)
      p.setXYZ(1, b.x, b.y, b.z)
      p.needsUpdate = true
      line.computeLineDistances()
    }
  }

  /** Outline objects in colours of their own (Map id -> colour), or none. */
  setHighlights(marks) {
    this.highlights = marks ?? new Map()
    this.setSelection(this.selected, this.face)
  }

  setEditState(state) {
    this.edit = state
    this.setSelection(this.selected, this.face)
  }

  /** Base-mesh vertex indices covered by the edit selection. */
  editVertices() {
    const ed = this.edit
    const node = this.nodes.get(this.selected)
    if (!ed.active || !node) return []
    const set = new Set()
    if (ed.mode === 'vertex') ed.verts.forEach((v) => set.add(v))
    if (ed.mode === 'edge') ed.edges.forEach(([a, b]) => (set.add(a), set.add(b)))
    if (ed.mode === 'face') ed.faces.forEach((f) => node.baseMesh.faces[f]?.forEach((v) => set.add(v)))
    return [...set].filter((v) => v < node.baseMesh.vertices.length)
  }

  placeGizmo() {
    if (this.sculpt.active) {
      if (this.gizmo.object) this.gizmo.detach()
      return
    }
    if (this.ik && this.ikHandle.visible && this.showGizmo !== false) {
      if (this.gizmo.object !== this.ikHandle) this.gizmo.attach(this.ikHandle)
      this.gizmo.setMode('translate')
      return
    }
    // showGizmo: true, false, or 'edit' (only for component moves in edit mode).
    const allowed = this.showGizmo === true || (this.showGizmo === 'edit' && this.edit.active)
    const node = allowed ? this.nodes.get(this.selected) : null
    if (node && this.edit.active) {
      const verts = this.editVertices()
      if (!verts.length) {
        if (this.gizmo.object) this.gizmo.detach()
        return
      }
      if (this.gizmo.dragging) return
      node.group.updateMatrixWorld(true)
      const c = new THREE.Vector3()
      for (const v of verts) c.add(new THREE.Vector3(...node.baseMesh.vertices[v]))
      c.multiplyScalar(1 / verts.length).applyMatrix4(node.group.matrixWorld)
      this.pivot.position.copy(c)
      this.pivot.quaternion.identity()
      this.pivot.scale.set(1, 1, 1)
      this.pivot.updateMatrixWorld(true)
      if (this.gizmo.object !== this.pivot) this.gizmo.attach(this.pivot)
      this.gizmo.setMode('translate')
      return
    }
    if (node && this.gizmo.object !== node.group) this.gizmo.attach(node.group)
    if (!node && this.gizmo.object) this.gizmo.detach()
  }

  /** The gizmo's movement since the drag began, in the object's local space. */
  localDelta() {
    const node = this.nodes.get(this.selected)
    if (!node || !this.pivotStart) return [0, 0, 0]
    const inv = node.group.matrixWorld.clone().invert()
    const a = this.pivotStart.clone().applyMatrix4(inv)
    const b = this.pivot.position.clone().applyMatrix4(inv)
    return b.sub(a).toArray()
  }

  previewMove() {
    const node = this.nodes.get(this.selected)
    if (!node) return
    const d = this.localDelta()
    const moved = new Set(this.editVertices())
    const base = node.data.mesh
    const preview = {
      vertices: base.vertices.map((v, i) => (moved.has(i) ? [v[0] + d[0], v[1] + d[1], v[2] + d[2]] : v)),
      faces: base.faces,
    }
    this.setCage(node, preview)
    if (!node.data.display) this.setMesh(node, preview)
  }

  /** Nearest vertex or edge of the selected object under the cursor, or a face. */
  pickComponent(clientX, clientY, mode) {
    const node = this.nodes.get(this.selected)
    if (!node) return null
    const rect = this.renderer.domElement.getBoundingClientRect()
    const ndc = new THREE.Vector2(((clientX - rect.left) / rect.width) * 2 - 1, -((clientY - rect.top) / rect.height) * 2 + 1)
    this.raycaster.setFromCamera(ndc, this.camera)
    const hit = this.raycaster.intersectObject(node.cage, false)[0]
    if (mode === 'face') return hit ? { face: node.triFace[hit.faceIndex] } : null
    node.group.updateMatrixWorld(true)
    const mesh = node.baseMesh
    const world = mesh.vertices.map((v) => new THREE.Vector3(...v).applyMatrix4(node.group.matrixWorld))
    const screen = world.map((p) => this.toClient(p, rect))
    // Hidden components are skipped: they must not be behind the surface hit.
    const limit = hit ? hit.distance + 0.02 * hit.distance + 1e-3 : Infinity
    const visible = (p) => this.camera.position.distanceTo(p) <= limit
    const mouse = { x: clientX, y: clientY }
    if (mode === 'vertex') {
      let best = null
      world.forEach((p, i) => {
        const d = Math.hypot(screen[i].x - mouse.x, screen[i].y - mouse.y)
        if (d < 14 && visible(p) && (!best || d < best.d)) best = { d, vertex: i }
      })
      return best
    }
    let best = null
    const seen = new Set()
    for (const f of mesh.faces) {
      for (let k = 0; k < f.length; k++) {
        const a = f[k]
        const b = f[(k + 1) % f.length]
        const id = a < b ? `${a},${b}` : `${b},${a}`
        if (seen.has(id)) continue
        seen.add(id)
        const mid = world[a].clone().add(world[b]).multiplyScalar(0.5)
        if (!visible(mid)) continue
        const d = segmentDistance(mouse, screen[a], screen[b])
        if (d < 10 && (!best || d < best.d)) best = { d, edge: [a, b] }
      }
    }
    return best
  }

  toClient(p, rect) {
    const q = p.clone().project(this.camera)
    return { x: rect.left + ((q.x + 1) / 2) * rect.width, y: rect.top + ((1 - q.y) / 2) * rect.height }
  }

  // -- sculpt ---------------------------------------------------------------

  setSculptState(state) {
    this.sculpt = state
    if (!state.active) this.brushRing.visible = false
    this.placeGizmo()
  }

  /** The selected object's surface under the cursor (world point and normal). */
  sculptHit(clientX, clientY) {
    const node = this.nodes.get(this.selected)
    if (!node) return null
    this.aim(clientX, clientY)
    node.group.updateMatrixWorld(true)
    const hit = this.raycaster.intersectObject(node.mesh, false)[0]
    if (!hit) return null
    const normal = hit.face.normal.clone().transformDirection(node.mesh.matrixWorld)
    if (normal.dot(this.raycaster.ray.direction) > 0) normal.negate()
    return { node, point: hit.point, normal }
  }

  /** Point the raycaster through a screen position. */
  aim(clientX, clientY) {
    const rect = this.renderer.domElement.getBoundingClientRect()
    const ndc = new THREE.Vector2(((clientX - rect.left) / rect.width) * 2 - 1, -((clientY - rect.top) / rect.height) * 2 + 1)
    this.raycaster.setFromCamera(ndc, this.camera)
  }

  placeRing(hit) {
    const ring = this.brushRing
    ring.visible = Boolean(hit) && this.sculpt.active
    if (!ring.visible) return
    ring.position.copy(hit.point).addScaledVector(hit.normal, 0.002)
    ring.quaternion.setFromUnitVectors(new THREE.Vector3(0, 0, 1), hit.normal)
    ring.scale.setScalar(this.sculpt.radius)
    ring.material.color.set(this.stroke ? SELECT : 0xffffff)
  }

  /** Start a stroke if the press lands on the selected object. */
  sculptDown(clientX, clientY, mods = {}) {
    if (!this.sculpt.active) return false
    const hit = this.sculptHit(clientX, clientY)
    if (!hit) return false
    const { node } = hit
    const s = this.sculpt
    const g = node.group
    const scale = (Math.abs(g.scale.x) + Math.abs(g.scale.y) + Math.abs(g.scale.z)) / 3
    const opts = {
      brush: mods.shiftKey ? 'smooth' : s.brush,
      radius: s.radius / scale,
      strength: s.strength,
      invert: Boolean(s.invert) !== Boolean(mods.ctrlKey || mods.metaKey),
      symmetry: s.symmetry || null,
      // Dynamic topology: the detail size follows the brush.
      detail: s.dynamic ? (s.radius * s.detail) / scale : null,
    }
    const inv = g.matrixWorld.clone().invert()
    const local = (p) => p.clone().applyMatrix4(inv).toArray()
    const plane = new THREE.Plane().setFromNormalAndCoplanarPoint(this.camera.getWorldDirection(new THREE.Vector3()), hit.point)
    this.stroke = { node, opts, local, plane, start: hit.point.clone(), offset: null, path: createStroke(node.data.mesh, opts), dirty: true }
    this.stroke.path.add(local(hit.point))
    this.controls.enabled = false
    this.placeRing(hit)
    return true
  }

  sculptMove(clientX, clientY) {
    if (!this.sculpt.active) return
    const st = this.stroke
    if (st && st.opts.brush === 'grab') {
      this.aim(clientX, clientY)
      const q = this.raycaster.ray.intersectPlane(st.plane, new THREE.Vector3())
      if (!q) return
      const a = st.local(st.start)
      const b = st.local(q)
      st.offset = [b[0] - a[0], b[1] - a[1], b[2] - a[2]]
      st.path.grab(st.offset)
      st.dirty = true
      this.brushRing.position.copy(q)
      return
    }
    const hit = this.sculptHit(clientX, clientY)
    this.placeRing(hit)
    if (st && hit && hit.node === st.node) {
      st.path.add(st.local(hit.point))
      st.dirty = true
    }
  }

  sculptUp() {
    const st = this.stroke
    if (!st) return
    this.stroke = null
    this.controls.enabled = true
    this.brushRing.material.color.set(0xffffff)
    this.flushStroke(st)
    const r = (v) => v.map((x) => Math.round(x * 1e5) / 1e5)
    const { brush, radius, strength, invert, symmetry, detail } = st.opts
    if (brush === 'grab' && !st.offset) return
    st.node.sculptPreview = true
    const cmd = { brush, points: st.path.points.map(r), radius: Math.round(radius * 1e5) / 1e5, strength, invert }
    if (symmetry) cmd.symmetry = symmetry
    if (detail && brush !== 'grab') cmd.detail = Math.round(detail * 1e5) / 1e5
    if (brush === 'grab') cmd.offset = r(st.offset)
    this.onSculpt(st.node.id, cmd)
  }

  flushStroke(st = this.stroke) {
    if (!st || !st.dirty) return
    st.dirty = false
    this.setMesh(st.node, { vertices: st.path.vertices(), faces: st.path.faces() }, null)
  }

  /** Drop a preview the engine did not accept. */
  revertSculpt(id) {
    const node = this.nodes.get(id)
    if (!node) return
    node.sculptPreview = false
    this.setMesh(node, displayMesh(node.data))
  }

  /** What an object shows at the current frame: its displayed mesh, posed by its bones. */
  shownMesh(o) {
    // Viewport-only camera body and viewing pyramid; not part of scene geometry.
    if (o.light) return {vertices:[[0,.08,0],[.08,0,0],[0,0,.08],[-.08,0,0],[0,0,-.08],[0,-.08,0]],faces:[[0,2,1],[0,3,2],[0,4,3],[0,1,4],[5,1,2],[5,2,3],[5,3,4],[5,4,1]]}
    if (o.camera?.ortho_height != null) return {vertices:[[-.3,-.2,0],[.3,-.2,0],[.3,.2,0],[-.3,.2,0],[-.3,-.2,-.45],[.3,-.2,-.45],[.3,.2,-.45],[-.3,.2,-.45]],faces:[[0,3,2,1],[0,1,5,4],[1,2,6,5],[2,3,7,6],[3,0,4,7]]}
    if (o.camera) return { vertices: [[-.14,-.1,0],[.14,-.1,0],[.14,.1,0],[-.14,.1,0],[-.3,-.2,-.45],[.3,-.2,-.45],[.3,.2,-.45],[-.3,.2,-.45]], faces: [[0,3,2,1],[0,1,5,4],[1,2,6,5],[2,3,7,6],[3,0,4,7]] }
    return posedMesh(this.rigPose(o), displayMesh(o), this.displayFrame())
  }

  /** Draw a rigged object's bones (posed), the chosen one highlighted. */
  setBones(node) {
    const o = node.data
    // three.js only skips objects whose `visible` is exactly false.
    const show = Boolean(hasRig(o) && (node.id === this.selected || this.showBones))
    node.bones.visible = node.boneEdges.visible = show
    if (!show) return
    const rig = this.rigPose(o)
    const key = JSON.stringify([rig.bones, this.displayFrame(), (o.tracks || []).filter((t) => t.property === 'bone'), this.bone])
    if (key === node.boneKey) return
    node.boneKey = key
    const pos = []
    const col = []
    const edges = []
    const segments = boneSegments(rig, this.displayFrame())
    o.bones.forEach((b, i) => {
      const [head, tail] = segments[i]
      const axis = tail.clone().sub(head)
      const len = axis.length()
      if (len < 1e-9) return
      const d = axis.clone().normalize()
      const u = new THREE.Vector3(...(Math.abs(d.y) < 0.9 ? [0, 1, 0] : [1, 0, 0])).cross(d).normalize()
      const v = d.clone().cross(u)
      const w = len * 0.1
      const mid = head.clone().addScaledVector(d, len * 0.18)
      const ring = [u, v, u.clone().negate(), v.clone().negate()].map((x) => mid.clone().addScaledVector(x, w))
      const c = new THREE.Color(b.name === this.bone ? 0xff8a4c : 0x9fc6ff)
      for (let k = 0; k < 4; k++) {
        const [a, n] = [ring[k], ring[(k + 1) % 4]]
        for (const p of [head, a, n, tail, n, a]) {
          pos.push(p.x, p.y, p.z)
          col.push(c.r, c.g, c.b)
        }
        edges.push(head.x, head.y, head.z, a.x, a.y, a.z, a.x, a.y, a.z, tail.x, tail.y, tail.z, a.x, a.y, a.z, n.x, n.y, n.z)
      }
    })
    node.bones.geometry.dispose()
    node.bones.geometry = new THREE.BufferGeometry()
      .setAttribute('position', new THREE.Float32BufferAttribute(pos, 3))
      .setAttribute('color', new THREE.Float32BufferAttribute(col, 3))
    node.boneEdges.geometry.dispose()
    node.boneEdges.geometry = new THREE.BufferGeometry().setAttribute('position', new THREE.Float32BufferAttribute(edges, 3))
    if (this.ik?.id === node.id && !node.ikStart) this.placeIkHandle()
  }

  /** Show bone `name` of object `id` turned to `rotation` before the edit is committed. */
  previewBone(id, name, rotation) {
    const node = this.nodes.get(id)
    if (!node) return
    const o = node.data
    // The next scene update replaces this preview with the real object.
    node.data = {
      ...o,
      bones: o.bones.map((b) => (b.name === name ? { ...b, rotation } : b)),
      tracks: (o.tracks || []).filter((t) => !(t.property === 'bone' && t.bone === name)),
    }
    this.setMesh(node, this.shownMesh(node.data), null)
    node.key = null
    this.setBones(node)
  }

  /** Show the reach handle on bone `ik.bone` of object `ik.id`, or none. */
  setIk(ik) {
    this.ik = ik
    this.placeIkHandle()
    this.placeGizmo()
  }

  placeIkHandle() {
    const node = this.ik && this.nodes.get(this.ik.id)
    const k = node ? node.data.bones?.findIndex((b) => b.name === this.ik.bone) : -1
    this.ikHandle.visible = k >= 0
    if (k < 0 || this.gizmo.dragging) return
    node.group.updateMatrixWorld(true)
    const [, tail] = boneSegments(this.rigPose(node.data), this.displayFrame())[k]
    this.ikHandle.position.copy(tail.applyMatrix4(node.group.matrixWorld))
  }

  /** Bend the chain so the tip reaches `world` (a preview until committed). */
  dragIk(world) {
    const node = this.ik && this.nodes.get(this.ik.id)
    if (!node) return
    if (world !== this.ikHandle.position) this.ikHandle.position.copy(world)
    const o = node.data
    const end = o.bones.findIndex((b) => b.name === this.ik.bone)
    if (end < 0) return
    node.ikStart ||= o
    const start = node.ikStart
    node.group.updateMatrixWorld(true)
    const local = world.clone().applyMatrix4(node.group.matrixWorld.clone().invert())
    const rot = reach(start.bones, boneRotations(start, this.currentFrame), end, Infinity, local)
    node.data = {
      ...start,
      bones: start.bones.map((b, i) => ({ ...b, rotation: rot[i] })),
      tracks: (start.tracks || []).filter((t) => t.property !== 'bone'),
    }
    this.setMesh(node, this.shownMesh(node.data), null)
    node.key = null
    this.setBones(node)
  }

  /** Commit the handle's position as a `reach`. */
  commitIk() {
    const node = this.ik && this.nodes.get(this.ik.id)
    if (!node || !node.ikStart) return
    node.ikStart = null
    this.onReach?.(this.ik.id, this.ik.bone, this.ikHandle.position.toArray().map((v) => Math.round(v * 1e5) / 1e5))
  }

  /** Highlight a bone of the selected object (by name), or none. */
  setBone(name) {
    this.bone = name
    const node = this.nodes.get(this.selected)
    if (node) this.setBones(node)
  }

  displayFrame() { return this.frameConstrained && this.framePoses ? this.resolvedFrame : this.currentFrame }

  rigPose(o) {
    const bones = this.framePoses?.get(o.id)?.bones
    return bones ? { ...o, bones, tracks: (o.tracks || []).filter(t => t.property !== 'bone') } : o
  }

  objectPose(o) {
    return this.framePoses?.get(o.id) || pose(o, this.currentFrame)
  }

  /** Keep one frame request in flight, coalescing playback to the latest frame. */
  setFrame(frame) {
    this.currentFrame = frame
    if (this.frameConstrained) {
      const key = `${this.frameEpoch}:${this.frameRevision}:${frame}`
      if (this.frameRequested !== key) {
        this.frameRequested = key
        this.pendingFrame = { frame, revision: this.frameRevision, key, epoch: this.frameEpoch, preview: this.framePreview }
        if (!this.frameInFlight) this.framePromise = this.requestFrame()
      }
      return
    }
    this.applyFrame()
  }

  async requestFrame() {
    if (this.frameInFlight || !this.pendingFrame) return
    const request = this.pendingFrame
    this.pendingFrame = null
    this.frameInFlight = true
    try {
      const response = await fetch(`/api/frame?frame=${request.frame}`, request.preview ? {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(request.preview)} : {})
      const result = await response.json()
      if (!response.ok) throw new Error(result.error || 'Cannot evaluate animation frame')
      if (this.frameConstrained && result.revision === this.frameRevision && request.revision === this.frameRevision && request.epoch === this.frameEpoch) {
        // Playback may advance during the request. Show the whole solved frame
        // atomically, then fetch the newest one; seeking discards older replies.
        if (!this.pendingFrame || this.playing) {
          this.framePoses = new Map(result.objects.map(o => [o.id, o]))
          this.resolvedFrame = result.frame
          this.applyFrame()
          this.onFrameResolved?.()
        }
      }
    } catch (e) {
      if (request.epoch === this.frameEpoch && (request.key === this.frameRequested || this.playing)) this.onFrameError?.(e.message)
    } finally {
      this.frameInFlight = false
      if (this.pendingFrame) this.framePromise = this.requestFrame()
    }
  }

  applyFrame() {
    for (const node of this.nodes.values()) {
      if ((!this.frameConstrained && !isAnimated(node.data)) || (!this.frameConstrained && this.anim.has(`tf:${node.id}`))) continue
      // A delayed SSE refresh may start a cosmetic tween back to rest.
      // Once the core frame arrives, it owns the transform and material.
      if (this.frameConstrained) {
        this.anim.items.get(`tf:${node.id}`)?.finish(false)
        this.anim.items.get(`mat:${node.id}`)?.finish(false)
      }
      const p = this.objectPose(node.data)
      this.setTransform(node.group, p.transform)
      this.setMaterial(node, p.material)
      if (hasRig(node.data)) {
        const shown = this.shownMesh(node.data)
        const key = geometryKey(node.data, shown)
        if (key !== node.key) this.setMesh(node, shown, key)
        this.setBones(node)
      }
    }
    if (this.edit.active && !this.gizmo.dragging) this.placeGizmo()
  }

  setGizmoMode(mode) {
    this.gizmo.setMode(mode)
  }

  setWireframe(on) {
    this.wireframe = on
    this.applyWireframe()
  }

  applyWireframe() {
    for (const node of this.nodes.values()) node.wire.visible = this.wireframe
  }

  pick(clientX, clientY) {
    const rect = this.renderer.domElement.getBoundingClientRect()
    const ndc = new THREE.Vector2(((clientX - rect.left) / rect.width) * 2 - 1, -((clientY - rect.top) / rect.height) * 2 + 1)
    this.raycaster.setFromCamera(ndc, this.camera)
    const meshes = [...this.nodes.values()].map((n) => n.cage)
    const hit = this.raycaster.intersectObjects(meshes, false)[0]
    if (!hit) return null
    const id = hit.object.userData.id
    return { id, face: this.nodes.get(id).triFace[hit.faceIndex] }
  }

  // -- camera ---------------------------------------------------------------

  viewPeerCamera({ eye, target, fov, ortho_height, up }) {
    this.anim.items.get('camera')?.finish(false)
    this.spinRate = 0
    // Flush any residual orbit damping before installing the peer pose.
    const damping = this.controls.enableDamping
    this.controls.enableDamping = false
    this.controls.update()
    this.switchProjection(ortho_height)
    this.camera.up.fromArray(up || [0,1,0])
    this.camera.position.fromArray(eye)
    this.controls.target.fromArray(target)
    if (!this.camera.isOrthographicCamera) this.camera.fov = fov
    this.camera.updateProjectionMatrix()
    this.controls.update()
    this.controls.enableDamping = damping
  }

  effectiveOrthoHeight() {
    return this.camera.isOrthographicCamera ? (this.camera.top-this.camera.bottom)/this.camera.zoom : null
  }

  cameraSnapshot() {
    return {eye:this.camera.position.toArray(), target:this.controls.target.toArray(),
      up:this.camera.up.toArray(), fov:this.camera.fov ?? 36, aspect:this.camera.aspect,
      ...(this.camera.isOrthographicCamera ? {ortho_height:this.effectiveOrthoHeight()} : {})}
  }

  setOrthoExtent(height) {
    this.orthoHeight = height
    this.controls.minZoom=height/10000; this.controls.maxZoom=height/.001
    this.camera.top=height/2; this.camera.bottom=-height/2
    this.camera.left=-height*this.camera.aspect/2; this.camera.right=height*this.camera.aspect/2
    this.camera.updateProjectionMatrix()
  }

  switchProjection(height) {
    const old=this.camera
    const next=height != null ? (this.orthographicCamera ||= new THREE.OrthographicCamera(-1,1,1,-1,.05,200)) : this.perspectiveCamera
    if (next !== old) {
      next.position.copy(old.position); next.quaternion.copy(old.quaternion); next.up.copy(old.up)
      next.aspect=old.aspect; next.zoom=1
      this.camera=next; this.controls.object=next; this.gizmo.camera=next
      for (const composer of [this.composer,this.glowComposer]) {
        for (const pass of composer?.passes || []) if (pass instanceof RenderPass) pass.camera=next
      }
    }
    if (height != null) { next.zoom=1; this.setOrthoExtent(height) }
    else next.updateProjectionMatrix()
  }

  /** Return to the saved orbit, or look through an animated scene camera. */
  lookThrough(id) {
    if (id == null) {
      this.sceneCameraId = null
      this.switchProjection(this.savedCameraHeight)
      this.camera.up.fromArray(this.savedCameraUp || [0,1,0])
      if (!this.camera.isOrthographicCamera) this.camera.fov = this.savedCameraFov ?? 36
      this.controls.enabled = true
      if (this.savedCameraOrbit) this.setOrbit(this.savedCameraOrbit)
      this.camera.updateProjectionMatrix()
      for (const n of this.nodes.values()) n.group.visible = true
      return
    }
    if (this.sceneCameraId == null) {
      this.savedCameraOrbit = this.getOrbit()
      this.savedCameraFov = this.camera.fov ?? 36
      this.savedCameraHeight = this.effectiveOrthoHeight()
      this.savedCameraUp = this.camera.up.toArray()
    }
    this.sceneCameraId = id
    this.gizmo.detach()
    this.applySceneCamera()
  }

  applySceneCamera() {
    if (this.sceneCameraId == null) return
    const node = this.nodes.get(this.sceneCameraId)
    if (!node?.data.camera) return this.lookThrough(null)
    const posed = this.objectPose(node.data)
    const t = posed.transform
    this.switchProjection(posed.camera.ortho_height)
    const q = new THREE.Quaternion().setFromEuler(new THREE.Euler(...t.rotation, 'XYZ'))
    this.camera.position.fromArray(t.translation)
    this.camera.up.copy(new THREE.Vector3(0, 1, 0).applyQuaternion(q))
    this.camera.quaternion.copy(q)
    this.controls.target.copy(this.camera.position).add(new THREE.Vector3(0, 0, -posed.camera.focus).applyQuaternion(q))
    if (!this.camera.isOrthographicCamera) this.camera.fov = posed.camera.fov
    this.camera.updateProjectionMatrix()
    this.controls.enabled = false
    this.gizmo.detach()
    for (const n of this.nodes.values()) n.group.visible = n.id !== this.sceneCameraId
  }

  getOrbit() {
    const t = this.controls.target
    const off = this.camera.position.clone().sub(t)
    const distance = off.length()
    return {
      azimuth: Math.atan2(off.x, off.z) / DEG,
      elevation: Math.asin(off.y / distance) / DEG,
      distance,
      target: t.toArray(),
    }
  }

  setOrbit({ azimuth, elevation, distance, target }) {
    const t = new THREE.Vector3(...target)
    const az = azimuth * DEG
    const el = elevation * DEG
    this.controls.target.copy(t)
    this.camera.position.set(
      t.x + distance * Math.cos(el) * Math.sin(az),
      t.y + distance * Math.sin(el),
      t.z + distance * Math.cos(el) * Math.cos(az),
    )
    this.camera.lookAt(t)
  }

  orbitTo(to, ms = 900) {
    const from = this.getOrbit()
    const goal = { ...from }
    for (const [k, v] of Object.entries(to)) if (v !== undefined) goal[k] = v
    let dAz = goal.azimuth - from.azimuth
    if (!to.longWay) dAz = ((((dAz + 180) % 360) + 360) % 360) - 180
    return this.anim.add('camera', ms, (t) => {
      this.setOrbit({
        azimuth: from.azimuth + dAz * t,
        elevation: from.elevation + (goal.elevation - from.elevation) * t,
        distance: from.distance + (goal.distance - from.distance) * t,
        target: from.target.map((v, i) => v + (goal.target[i] - v) * t),
      })
    })
  }

  /** Bounds of the scene using each object's final (not animated) transform. */
  sceneBounds() {
    const box = new THREE.Box3()
    for (const node of this.nodes.values()) {
      const tf = this.objectPose(node.data).transform
      const m = new THREE.Matrix4().compose(
        new THREE.Vector3(...tf.translation),
        new THREE.Quaternion().setFromEuler(new THREE.Euler(...tf.rotation, 'XYZ')),
        new THREE.Vector3(...tf.scale),
      )
      for (const v of displayMesh(node.data).vertices) box.expandByPoint(new THREE.Vector3(...v).applyMatrix4(m))
    }
    return box
  }

  frameAll(ms = 700, { padding = 1.25, elevation, azimuth } = {}) {
    const box = this.sceneBounds()
    if (box.isEmpty()) return this.orbitTo({ target: [0, 0.5, 0], distance: 6.5, elevation, azimuth }, ms)
    const sphere = box.getBoundingSphere(new THREE.Sphere())
    if (this.camera.isOrthographicCamera) {
      this.camera.zoom=1
      this.setOrthoExtent(Math.max(.001, Math.min(10000,sphere.radius*2*padding/Math.min(1,this.camera.aspect))))
    }
    const fov = Math.min(this.camera.fov ?? 36, (this.camera.fov ?? 36) * this.camera.aspect) * DEG
    const distance = Math.max(1.2, (sphere.radius * padding) / Math.sin(fov / 2))
    return this.orbitTo({ target: sphere.center.toArray(), distance, elevation, azimuth }, ms)
  }

  /** Advisory peer outlines and camera frusta, outside the model root. */
  setPresence(peers) {
    if (this.peerRoot) {
      this.scene.remove(this.peerRoot)
      this.peerRoot.traverse((o) => { o.geometry?.dispose(); o.material?.dispose() })
    }
    this.peerRoot = new THREE.Group()
    this.scene.add(this.peerRoot)
    for (const p of peers) {
      for (const id of p.selection) {
        const node = this.nodes.get(id)
        if (!node) continue
        node.group.updateWorldMatrix(true, true)
        const box = new THREE.Box3().setFromObject(node.group)
        const helper = new THREE.Box3Helper(box, new THREE.Color(p.color))
        helper.material.depthTest = false
        helper.renderOrder = 9
        this.peerRoot.add(helper)
      }
      if (p.camera) {
        const h=p.camera.ortho_height, a=p.camera.aspect ?? 1.6
        const cam = h != null ? new THREE.OrthographicCamera(-h*a/2,h*a/2,h/2,-h/2,.08,.45) : new THREE.PerspectiveCamera(p.camera.fov,a,.08,.45)
        cam.up.fromArray(p.camera.up || [0,1,0])
        cam.position.fromArray(p.camera.eye)
        cam.lookAt(new THREE.Vector3(...p.camera.target))
        cam.updateMatrixWorld()
        const helper = new THREE.CameraHelper(cam)
        const colors = helper.geometry.getAttribute('color')
        const c = new THREE.Color(p.color)
        for (let i = 0; i < colors.count; i++) colors.setXYZ(i, c.r, c.g, c.b)
        colors.needsUpdate = true
        helper.material.depthTest = false
        helper.renderOrder = 9
        this.peerRoot.add(helper)
      }
    }
  }

  /** Page (client) coordinates of a world point given as [x, y, z]. */
  clientOf(p) {
    const rect = this.renderer.domElement.getBoundingClientRect()
    const q = new THREE.Vector3(...p).project(this.camera)
    return { x: rect.left + ((q.x + 1) / 2) * rect.width, y: rect.top + ((1 - q.y) / 2) * rect.height }
  }

  worldToScreen(v) {
    const p = v.clone().project(this.camera)
    return { x: ((p.x + 1) / 2) * this.el.clientWidth, y: ((1 - p.y) / 2) * this.el.clientHeight }
  }

  /** World-space position of a base-mesh vertex or the midpoint of an edge. */
  anchorComponent(id, { vertex, edge }) {
    const node = this.nodes.get(id)
    if (!node) return null
    node.group.updateMatrixWorld(true)
    const v = node.baseMesh.vertices
    const local = vertex !== undefined ? new THREE.Vector3(...v[vertex]) : new THREE.Vector3(...v[edge[0]]).add(new THREE.Vector3(...v[edge[1]])).multiplyScalar(0.5)
    return local.applyMatrix4(node.group.matrixWorld)
  }

  /** World-space centre of an object, or of one of its faces. */
  anchor(id, face) {
    const node = this.nodes.get(id)
    if (!node) return null
    node.group.updateMatrixWorld(true)
    const mesh = node.baseMesh
    const idx = face != null ? mesh.faces[face] : null
    let p
    if (idx) {
      p = new THREE.Vector3()
      for (const i of idx) p.add(new THREE.Vector3(...mesh.vertices[i]))
      p.multiplyScalar(1 / idx.length)
    } else {
      const box = new THREE.Box3().setFromBufferAttribute(node.mesh.geometry.attributes.position)
      p = box.getCenter(new THREE.Vector3())
    }
    return p.applyMatrix4(node.group.matrixWorld)
  }

  /** Scene lights are viewport helpers plus actual physically attenuated lights. */
  syncSceneLights() {
    for (const node of this.nodes.values()) {
      const lamp = node.data.light ? this.objectPose(node.data).light : null
      if (!lamp) continue
      if (!node.light || node.lightKind !== lamp.kind) {
        if (node.light) { this.scene.remove(node.light, node.light.target); node.light.dispose() }
        node.light = lamp.kind === 'sun' ? new THREE.DirectionalLight() : lamp.kind === 'spot' ? new THREE.SpotLight() : new THREE.PointLight()
        node.lightKind = lamp.kind
        node.light.castShadow = true
        node.light.shadow.mapSize.set(512,512)
        node.light.shadow.bias = -.0005
        node.light.shadow.camera.near = .01
        node.light.shadow.camera.far = 1000
        this.scene.add(node.light)
        if (node.light.target) this.scene.add(node.light.target)
      }
      const t = this.objectPose(node.data).transform
      const light = node.light
      light.color.set(lamp.color)
      light.intensity = lamp.intensity
      if (lamp.kind !== 'sun') light.distance = lamp.range ?? 0
      if (lamp.kind === 'spot') {
        light.angle = lamp.outer_cone ?? Math.PI/4
        light.penumbra = 1 - (lamp.inner_cone ?? 0) / light.angle
        const q = new THREE.Quaternion().setFromEuler(new THREE.Euler(...t.rotation, 'XYZ'))
        light.target.position.fromArray(t.translation).add(new THREE.Vector3(0,0,-1).applyQuaternion(q))
        light.shadow.camera.updateProjectionMatrix()
      }
      light.visible = lamp.intensity > 0
      node.mesh.material.color.set(lamp.color)
      node.mesh.material.emissive.set(lamp.color)
      node.mesh.material.emissiveIntensity = .8
      if (lamp.kind === 'sun') {
        const box=this.sceneBounds()
        const sphere=box.isEmpty() ? new THREE.Sphere(new THREE.Vector3(0,.5,0),2) : box.getBoundingSphere(new THREE.Sphere())
        const q=new THREE.Quaternion().setFromEuler(new THREE.Euler(...t.rotation,'XYZ'))
        const toward=new THREE.Vector3(0,0,1).applyQuaternion(q)
        const distance=Math.max(10,sphere.radius*2+1)
        light.position.copy(sphere.center).addScaledVector(toward,distance)
        light.target.position.copy(sphere.center)
        const radius=Math.max(2,sphere.radius*1.5)
        Object.assign(light.shadow.camera,{left:-radius,right:radius,bottom:-radius,top:radius,far:distance+sphere.radius*2+10})
        light.shadow.camera.updateProjectionMatrix()
      } else light.position.fromArray(t.translation)
    }
  }

  /** Offscreen diagnostic compiles the actual production light shader. */
  spotAttenuationProbe(coneCosine, penumbraCosine, angleCosine) {
    if (![coneCosine,penumbraCosine,angleCosine].every(Number.isFinite)) throw new Error('Probe inputs must be finite')
    const source = THREE.ShaderChunk.lights_pars_begin.match(spotFunction)?.[0]
    if (!source) throw new Error('Spot shader function missing')
    const target = new THREE.WebGLRenderTarget(1,1)
    const material = new THREE.ShaderMaterial({
      uniforms:{cone:{value:coneCosine},penumbra:{value:penumbraCosine},angle:{value:angleCosine}},
      vertexShader:'void main(){gl_Position=vec4(position,1.0);}',
      fragmentShader:`uniform float cone; uniform float penumbra; uniform float angle; ${source}\nvoid main(){float a=getSpotAttenuation(cone,penumbra,angle);gl_FragColor=vec4(a,a,a,1.0);}`,
      depthTest:false,depthWrite:false,
    })
    const geometry = new THREE.PlaneGeometry(2,2)
    const scene = new THREE.Scene();scene.add(new THREE.Mesh(geometry,material))
    const previous = this.renderer.getRenderTarget()
    try {
      this.renderer.setRenderTarget(target)
      this.renderer.render(scene,new THREE.OrthographicCamera(-1,1,1,-1,0,1))
      const pixel = new Uint8Array(4)
      this.renderer.readRenderTargetPixels(target,0,0,1,1,pixel)
      return Array.from(pixel)
    } finally {
      this.renderer.setRenderTarget(previous);target.dispose();geometry.dispose();material.dispose()
    }
  }

  // -- frame ----------------------------------------------------------------

  frame() {
    const now = this.clock.now()
    const dt = Math.min(100, now - this.lastFrame)
    this.lastFrame = now
    if (this.sceneCameraId == null && this.spinRate && !this.anim.has('camera')) {
      const o = this.getOrbit()
      this.setOrbit({ ...o, azimuth: o.azimuth + (this.spinRate * dt) / 1000 })
    }
    if (this.sceneCameraId != null) this.applySceneCamera()
    else this.controls.update()
    this.updateProjectionBackground()
    this.flushStroke()
    this.syncSceneLights()
    this.placeLinks()
    if (this.glowing()) {
      this.bloom()
      this.renderGlow()
      this.composer.render()
    } else this.renderer.render(this.scene, this.camera)
    this.drawAxes()
  }

  drawAxes() {
    const el = document.getElementById('axes')
    if (!el) return
    const q = this.camera.quaternion.clone().invert()
    const axes = [
      ['X', new THREE.Vector3(1, 0, 0), '#ff5d6c'],
      ['Y', new THREE.Vector3(0, 1, 0), '#8fd14f'],
      ['Z', new THREE.Vector3(0, 0, 1), '#4f9dff'],
    ].map(([n, v, c]) => ({ n, v: v.applyQuaternion(q), c }))
    axes.sort((a, b) => a.v.z - b.v.z)
    const r = 22
    el.innerHTML = `<svg viewBox="-34 -34 68 68">${axes
      .map(
        ({ n, v, c }) =>
          `<line x1="0" y1="0" x2="${v.x * r}" y2="${-v.y * r}" stroke="${c}" stroke-width="2" opacity="${v.z < -0.2 ? 0.45 : 1}"/>` +
          `<circle cx="${v.x * r}" cy="${-v.y * r}" r="7.5" fill="${c}" opacity="${v.z < -0.2 ? 0.45 : 1}"/>` +
          `<text x="${v.x * r}" y="${-v.y * r + 3.2}" text-anchor="middle">${n}</text>`,
      )
      .join('')}</svg>`
  }
}
