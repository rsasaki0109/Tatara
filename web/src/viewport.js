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
  constructor(el, clock, animator, { capture, onPick, onTransform, onMoveVertices }) {
    this.el = el
    this.clock = clock
    this.anim = animator
    this.capture = capture
    this.onPick = onPick
    this.onTransform = onTransform
    this.onMoveVertices = onMoveVertices
    // Edit mode: which components of the selected object are selected.
    this.edit = { active: false, mode: 'face', verts: [], edges: [], faces: [] }
    this.nodes = new Map()
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
    this.pivot = new THREE.Object3D()
    scene.add(this.pivot)
    this.gizmo.addEventListener('objectChange', () => {
      if (this.gizmo.object === this.pivot && this.gizmo.dragging) this.previewMove()
    })
    this.gizmo.addEventListener('dragging-changed', (e) => {
      this.controls.enabled = !e.value
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
        node.glowMat?.dispose()
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
    for (const x of [wire, outline, faceMark, cageLines, points, selPoints, selEdges]) {
      x.visible = false
      x.renderOrder = 2
    }
    cageLines.renderOrder = 3
    points.renderOrder = 4
    selEdges.renderOrder = 4
    selPoints.renderOrder = 5
    group.add(mesh, wire, outline, faceMark, cage, cageLines, points, selPoints, selEdges)
    this.root.add(group)
    const node = { id: o.id, group, mesh, wire, outline, faceMark, cage, cageLines, points, selPoints, selEdges, data: o, key: null, cageKey: null, triFace: [] }
    this.nodes.set(o.id, node)
    const posed = pose(o, this.currentFrame)
    this.setTransform(node.group, posed.transform)
    this.setMaterial(node, posed.material)
    this.setMesh(node, displayMesh(o))
    this.setCage(node, o.mesh)
    if (animate) {
      const target = node.group.scale.clone()
      this.anim.add(`tf:${o.id}`, 520, (t) => node.group.scale.copy(target).multiplyScalar(Math.max(0.0001, t)), ease.back)
    }
  }

  update(node, o, animate) {
    const prev = node.data
    node.data = o
    const plain = !o.display && !prev.display
    const key = meshKey(displayMesh(o))
    if (key !== node.key) {
      const morph = animate && plain && (this.morphStart(prev.mesh, o.mesh) || this.sameTopology(prev.mesh, o.mesh))
      if (morph) {
        const end = o.mesh.vertices
        const verts = end.map((v) => v.slice())
        this.anim.add(`mesh:${o.id}`, 480, (t) => {
          for (let i = 0; i < end.length; i++) {
            const s = morph[i]
            for (let k = 0; k < 3; k++) verts[i][k] = s[k] + (end[i][k] - s[k]) * t
          }
          const live = { vertices: verts, faces: o.mesh.faces }
          this.setMesh(node, live, t < 1 ? null : key)
          this.setCage(node, live, t < 1 ? null : meshKey(o.mesh))
        })
      } else {
        this.setMesh(node, displayMesh(o), key)
        if (animate) this.flash(node)
      }
    }
    if (meshKey(o.mesh) !== node.cageKey && !this.anim.has(`mesh:${o.id}`)) this.setCage(node, o.mesh)
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
          m.color.lerpColors(from.color, to.color, t)
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
    m.color.set(mat.color)
    m.roughness = mat.roughness
    m.metalness = mat.metalness
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

  setMesh(node, mesh, key = meshKey(mesh)) {
    const { geometry } = buildGeometry(mesh.vertices, mesh.faces)
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
    if (this.selected === node.id) this.setSelection(node.id, this.face)
  }

  setSelection(id, face) {
    this.selected = this.nodes.has(id) ? id : null
    this.face = this.selected != null ? face : null
    const ed = this.edit
    for (const node of this.nodes.values()) {
      const on = node.id === this.selected
      const editing = on && ed.active
      const mesh = node.baseMesh
      node.outline.visible = on && !editing
      node.cageLines.visible = editing || (on && Boolean(node.data.display))
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

  /** Pose every animated object at `frame`. */
  setFrame(frame) {
    this.currentFrame = frame
    for (const node of this.nodes.values()) {
      if (!isAnimated(node.data) || this.anim.has(`tf:${node.id}`)) continue
      const p = pose(node.data, frame)
      this.setTransform(node.group, p.transform)
      this.setMaterial(node, p.material)
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
      const tf = pose(node.data, this.currentFrame).transform
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
    const fov = Math.min(this.camera.fov, this.camera.fov * this.camera.aspect) * DEG
    const distance = Math.max(1.2, (sphere.radius * padding) / Math.sin(fov / 2))
    return this.orbitTo({ target: sphere.center.toArray(), distance, elevation, azimuth }, ms)
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
