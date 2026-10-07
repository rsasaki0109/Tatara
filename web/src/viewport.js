import * as THREE from 'three'
import { OrbitControls } from 'three/addons/controls/OrbitControls.js'
import { TransformControls } from 'three/addons/controls/TransformControls.js'
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js'
import { toCreasedNormals } from 'three/addons/utils/BufferGeometryUtils.js'
import { ease } from './clock.js'

const CREASE = THREE.MathUtils.degToRad(38)
const SELECT = 0xff8a4c
const DEG = Math.PI / 180

function meshKey(mesh) {
  let h = mesh.vertices.length * 31 + mesh.faces.length
  for (const v of mesh.vertices) h = (h * 1.000173 + v[0] * 3.1 + v[1] * 7.3 + v[2] * 11.7) % 1e9
  for (const f of mesh.faces) h = (h * 1.000071 + f.length + f[0]) % 1e9
  return h.toFixed(6)
}

function buildGeometry(vertices, faces) {
  const pos = []
  const triFace = []
  faces.forEach((f, fi) => {
    const a = vertices[f[0]]
    for (let k = 1; k < f.length - 1; k++) {
      const b = vertices[f[k]]
      const c = vertices[f[k + 1]]
      pos.push(a[0], a[1], a[2], b[0], b[1], b[2], c[0], c[1], c[2])
      triFace.push(fi)
    }
  })
  const g = new THREE.BufferGeometry()
  g.setAttribute('position', new THREE.Float32BufferAttribute(pos, 3))
  const geometry = toCreasedNormals(g, CREASE)
  g.dispose()
  geometry.computeBoundingSphere()
  return { geometry, triFace }
}

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

function faceGeometry(vertices, face) {
  const pos = []
  const a = vertices[face[0]]
  for (let k = 1; k < face.length - 1; k++) pos.push(...a, ...vertices[face[k]], ...vertices[face[k + 1]])
  const g = new THREE.BufferGeometry()
  g.setAttribute('position', new THREE.Float32BufferAttribute(pos, 3))
  return g
}

export class Viewport {
  constructor(el, clock, animator, { capture, onPick, onTransform }) {
    this.el = el
    this.clock = clock
    this.anim = animator
    this.capture = capture
    this.onPick = onPick
    this.onTransform = onTransform
    this.nodes = new Map()
    this.selected = null
    this.face = null
    this.wireframe = false
    this.spinRate = 0
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
    const pmrem = new THREE.PMREMGenerator(r)
    scene.environment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture
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

    const ground = new THREE.Mesh(new THREE.PlaneGeometry(60, 60), new THREE.ShadowMaterial({ opacity: 0.28 }))
    ground.rotation.x = -Math.PI / 2
    ground.receiveShadow = true
    scene.add(ground)
    const grid = new THREE.GridHelper(16, 32, 0x8a8f99, 0x5a5f69)
    grid.material.transparent = true
    grid.material.opacity = 0.16
    grid.material.depthWrite = false
    grid.position.y = 0.0005
    scene.add(grid)

    this.root = new THREE.Group()
    scene.add(this.root)

    const cam = (this.camera = new THREE.PerspectiveCamera(36, 1, 0.05, 200))
    this.controls = new OrbitControls(cam, r.domElement)
    this.controls.enableDamping = !capture
    this.controls.dampingFactor = 0.12
    this.setOrbit({ azimuth: 35, elevation: 22, distance: 6.5, target: [0, 0.5, 0] })

    this.gizmo = new TransformControls(cam, r.domElement)
    this.gizmo.setSize(0.85)
    this.gizmo.addEventListener('dragging-changed', (e) => {
      this.controls.enabled = !e.value
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
    let down = null
    r.domElement.addEventListener('pointerdown', (e) => {
      // A detached gizmo can keep a stale hovered axis.
      down = this.gizmo.object && this.gizmo.axis ? null : { x: e.clientX, y: e.clientY }
    })
    r.domElement.addEventListener('pointerup', (e) => {
      if (!down || e.button !== 0) return
      const moved = Math.hypot(e.clientX - down.x, e.clientY - down.y)
      down = null
      if (moved > 4) return
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
    this.renderer.domElement.style.width = `${w}px`
    this.renderer.domElement.style.height = `${h}px`
    this.camera.aspect = w / h
    this.camera.updateProjectionMatrix()
  }

  // -- scene sync -----------------------------------------------------------

  sync(scene, animate) {
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
      if (this.gizmo.object === node.group) this.gizmo.detach()
      const done = () => {
        this.root.remove(node.group)
        node.mesh.geometry.dispose()
      }
      if (animate) {
        const s = node.group.scale.clone()
        this.anim.add(`tf:${id}`, 260, (t) => node.group.scale.copy(s).multiplyScalar(Math.max(0.0001, 1 - t)), ease.inOut).then(done)
      } else done()
    }
    this.setSelection(this.selected, this.face)
    this.applyWireframe()
  }

  create(o, animate) {
    const group = new THREE.Group()
    group.userData.id = o.id
    const material = new THREE.MeshPhysicalMaterial({ clearcoatRoughness: 0.18 })
    const mesh = new THREE.Mesh(new THREE.BufferGeometry(), material)
    mesh.castShadow = true
    mesh.receiveShadow = true
    mesh.userData.id = o.id
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
    for (const x of [wire, outline, faceMark]) {
      x.visible = false
      x.renderOrder = 2
    }
    group.add(mesh, wire, outline, faceMark)
    this.root.add(group)
    const node = { id: o.id, group, mesh, wire, outline, faceMark, data: null, key: null, triFace: [] }
    this.nodes.set(o.id, node)
    this.setTransform(node.group, o.transform)
    this.setMaterial(node, o.material)
    this.setMesh(node, o.mesh)
    node.data = o
    if (animate) {
      const target = node.group.scale.clone()
      this.anim.add(`tf:${o.id}`, 520, (t) => node.group.scale.copy(target).multiplyScalar(Math.max(0.0001, t)), ease.back)
    }
  }

  update(node, o, animate) {
    const prev = node.data
    node.data = o
    const key = meshKey(o.mesh)
    if (key !== node.key) {
      const morph = animate && this.morphStart(prev.mesh, o.mesh)
      if (morph) {
        const end = o.mesh.vertices
        const verts = end.map((v) => v.slice())
        this.anim.add(`mesh:${o.id}`, 480, (t) => {
          for (let i = 0; i < end.length; i++) {
            const s = morph[i]
            for (let k = 0; k < 3; k++) verts[i][k] = s[k] + (end[i][k] - s[k]) * t
          }
          this.setMesh(node, { vertices: verts, faces: o.mesh.faces }, t < 1 ? null : key)
        })
      } else {
        this.setMesh(node, o.mesh, key)
        if (animate) this.flash(node)
      }
    }
    const tf = o.transform
    const g = node.group
    const same =
      g.position.distanceTo(new THREE.Vector3(...tf.translation)) < 1e-6 &&
      g.scale.distanceTo(new THREE.Vector3(...tf.scale)) < 1e-6 &&
      new THREE.Vector3(g.rotation.x, g.rotation.y, g.rotation.z).distanceTo(new THREE.Vector3(...tf.rotation)) < 1e-6
    if (!same && animate) {
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
    if (JSON.stringify(prev.material) !== JSON.stringify(o.material)) {
      if (animate) {
        const m = node.mesh.material
        const c0 = m.color.clone()
        const r0 = m.roughness
        const m0 = m.metalness
        const target = new THREE.MeshPhysicalMaterial()
        this.applyMaterial(target, o.material)
        this.anim.add(`mat:${o.id}`, 420, (t) => {
          m.color.lerpColors(c0, target.color, t)
          m.roughness = r0 + (target.roughness - r0) * t
          m.metalness = m0 + (target.metalness - m0) * t
          m.clearcoat = Math.max(0, 0.65 - m.roughness)
          if (t === 1) target.dispose()
        })
      } else this.setMaterial(node, o.material)
    }
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
    m.color.set(mat.color)
    m.roughness = mat.roughness
    m.metalness = mat.metalness
    m.clearcoat = Math.max(0, 0.65 - mat.roughness)
  }

  setMaterial(node, mat) {
    this.applyMaterial(node.mesh.material, mat)
  }

  setMesh(node, mesh, key = meshKey(mesh)) {
    const { geometry, triFace } = buildGeometry(mesh.vertices, mesh.faces)
    node.mesh.geometry.dispose()
    node.mesh.geometry = geometry
    node.triFace = triFace
    node.wire.geometry.dispose()
    node.wire.geometry = polygonEdges(mesh.vertices, mesh.faces)
    node.outline.geometry.dispose()
    node.outline.geometry = new THREE.EdgesGeometry(geometry, 32)
    if (key) node.key = key
    node.liveMesh = mesh
    if (this.selected === node.id && this.face != null) this.setSelection(node.id, this.face)
  }

  setSelection(id, face) {
    this.selected = this.nodes.has(id) ? id : null
    this.face = this.selected != null ? face : null
    for (const node of this.nodes.values()) {
      const on = node.id === this.selected
      node.outline.visible = on
      const mesh = node.liveMesh || node.data.mesh
      const showFace = on && this.face != null && this.face < mesh.faces.length
      node.faceMark.visible = showFace
      if (showFace) {
        node.faceMark.geometry.dispose()
        node.faceMark.geometry = faceGeometry(mesh.vertices, mesh.faces[this.face])
      }
    }
    const node = this.showGizmo ? this.nodes.get(this.selected) : null
    if (node && this.gizmo.object !== node.group) this.gizmo.attach(node.group)
    if (!node && this.gizmo.object) this.gizmo.detach()
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
    const meshes = [...this.nodes.values()].map((n) => n.mesh)
    const hit = this.raycaster.intersectObjects(meshes, false)[0]
    if (!hit) return null
    const id = hit.object.userData.id
    return { id, face: this.nodes.get(id).triFace[hit.faceIndex] }
  }

  // -- camera ---------------------------------------------------------------

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
      const tf = node.data.transform
      const m = new THREE.Matrix4().compose(
        new THREE.Vector3(...tf.translation),
        new THREE.Quaternion().setFromEuler(new THREE.Euler(...tf.rotation, 'XYZ')),
        new THREE.Vector3(...tf.scale),
      )
      for (const v of node.data.mesh.vertices) box.expandByPoint(new THREE.Vector3(...v).applyMatrix4(m))
    }
    return box
  }

  frameAll(ms = 700, { padding = 1.25, elevation, azimuth } = {}) {
    const box = this.sceneBounds()
    if (box.isEmpty()) return this.orbitTo({ target: [0, 0.5, 0], distance: 6.5, elevation, azimuth }, ms)
    const sphere = box.getBoundingSphere(new THREE.Sphere())
    const fov = Math.min(this.camera.fov, this.camera.fov * this.camera.aspect) * DEG
    const distance = Math.max(1.2, (sphere.radius * padding) / Math.sin(fov / 2))
    return this.orbitTo({ target: sphere.center.toArray(), distance, elevation, azimuth }, ms)
  }

  worldToScreen(v) {
    const p = v.clone().project(this.camera)
    return { x: ((p.x + 1) / 2) * this.el.clientWidth, y: ((1 - p.y) / 2) * this.el.clientHeight }
  }

  /** World-space centre of an object, or of one of its faces. */
  anchor(id, face) {
    const node = this.nodes.get(id)
    if (!node) return null
    node.group.updateMatrixWorld(true)
    const mesh = node.liveMesh || node.data.mesh
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

  // -- frame ----------------------------------------------------------------

  frame() {
    const now = this.clock.now()
    const dt = Math.min(100, now - this.lastFrame)
    this.lastFrame = now
    if (this.spinRate && !this.anim.has('camera')) {
      const o = this.getOrbit()
      this.setOrbit({ ...o, azimuth: o.azimuth + (this.spinRate * dt) / 1000 })
    }
    this.controls.update()
    this.renderer.render(this.scene, this.camera)
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
