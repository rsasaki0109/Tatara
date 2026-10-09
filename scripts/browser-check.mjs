#!/usr/bin/env node
// Browser smoke test against an isolated editor: real clicks and keys for
// editing, history and files, then every demo scenario in capture mode.
// Requires `npm --prefix web run build` and `cargo build --release`.

import { spawn } from 'node:child_process'
import fs from 'node:fs'
import net from 'node:net'
import path from 'node:path'
import { createRequire } from 'node:module'

// A 2x2 PNG (red, green / blue, white) for the image upload check.
const TINY_PNG = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEklEQVR4nGP4z8DAAMIM/4EAAB/uBfsL2WiLAAAAAElFTkSuQmCC',
  'base64',
)
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const { chromium } = createRequire(path.join(root, 'web/package.json'))('playwright-core')

function findBrowser() {
  if (process.env.TATARA_BROWSER_PATH) return process.env.TATARA_BROWSER_PATH
  const base = '/opt/pw-browsers'
  if (!fs.existsSync(base)) return undefined
  for (const d of fs.readdirSync(base).sort().reverse()) {
    const exe = path.join(base, d, 'chrome-linux/chrome')
    if (d.startsWith('chromium-') && fs.existsSync(exe)) return exe
  }
}

const port = await new Promise((resolve) => {
  const s = net.createServer().listen(0, '127.0.0.1', () => {
    const { port } = s.address()
    s.close(() => resolve(port))
  })
})
const url = `http://127.0.0.1:${port}`
const bin = path.join(root, 'target/release', process.platform === 'win32' ? 'tatara.exe' : 'tatara')
const server = spawn(bin, [], { env: { ...process.env, TATARA_PORT: String(port), TATARA_WEB_DIR: path.join(root, 'web/dist') }, stdio: 'inherit' })
for (let i = 0; i < 100; i++) {
  try {
    if ((await fetch(`${url}/api/state`)).ok) break
  } catch {}
  await new Promise((r) => setTimeout(r, 100))
}

const browser = await chromium.launch({ executablePath: findBrowser(), args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] })
let failed = false
// page.waitForFunction treats a returned promise as truthy, so waits on
// the server (fetch) poll here and await the page's answer. Then wait for
// the editor to show that revision too, so the next click does not act on
// a stale scene (and get a stale-revision error).
async function until(page, fn, arg) {
  const deadline = Date.now() + 30000
  const wait = async (what, ok) => {
    while (!(await ok())) {
      if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`)
      await page.waitForTimeout(50)
    }
  }
  await wait(fn, () => page.evaluate(fn, arg))
  await wait('the editor to catch up', () =>
    page.evaluate(() =>
      fetch('/api/scene')
        .then((r) => r.json())
        .then((s) => document.querySelector('#status-rev').textContent === `Revision ${s.revision}`),
    ),
  )
}
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`)
  if (!ok) failed = true
}

try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 720 }, acceptDownloads: true })
  const errors = []
  page.on('pageerror', (e) => errors.push(e.message))
  await page.goto(url)
  await page.evaluate(() => window.__tatara.ready)
  const count = () => page.locator('#outliner li').count()
  // New objects grow in over half a second; wait before clicking their faces.
  const settled = () => page.waitForFunction(() => window.__tatara.debug().anims.length === 0)
  const faces = async () => Number((await page.textContent('#status-mesh')).match(/([\d,]+) faces/)[1].replace(/,/g, ''))

  await page.click('[data-add=cube]')
  await page.waitForFunction(() => document.querySelectorAll('#outliner li').length === 1)
  check((await count()) === 1, 'toolbar adds a cube')
  await settled()

  await page.keyboard.press('Escape') // detach the gizmo from the cube's centre
  const box = await page.locator('#viewport > canvas[data-engine]').boundingBox()
  await page.keyboard.down('Alt')
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2)
  await page.keyboard.up('Alt')
  await page.waitForSelector('.face-info.on')
  check(true, 'alt+click selects a face')
  await page.click('#edit-group [data-action=extrude]')
  await page.waitForFunction(() => /10 faces/.test(document.querySelector('#status-mesh').textContent))
  check((await faces()) === 10, 'extrude adds four side faces')

  await page.keyboard.press('Escape')
  await page.keyboard.press('Control+z')
  await page.waitForFunction(() => /6 faces/.test(document.querySelector('#status-mesh').textContent))
  check((await faces()) === 6, 'undo restores the cube')
  await page.keyboard.press('Control+Shift+z')
  await page.waitForFunction(() => /10 faces/.test(document.querySelector('#status-mesh').textContent))
  check((await faces()) === 10, 'redo re-applies the extrusion')

  const [download] = await Promise.all([page.waitForEvent('download'), page.click('[data-action=save]')])
  const saved = JSON.parse(fs.readFileSync(await download.path(), 'utf8'))
  check(saved.objects.length === 1, 'save downloads the scene')
  await page.click('#outliner li')
  await page.click('[data-action=delete]')
  await page.waitForFunction(() => document.querySelectorAll('#outliner li').length === 0)
  await page.setInputFiles('#open-input', { name: 'scene.tatara.json', mimeType: 'application/json', buffer: Buffer.from(JSON.stringify(saved)) })
  await page.waitForFunction(() => document.querySelectorAll('#outliner li').length === 1)
  check(true, 'open restores a saved scene')
  const [obj] = await Promise.all([page.waitForEvent('download'), page.click('[data-action=exportObj]')])
  check(fs.readFileSync(await obj.path(), 'utf8').includes('o Cube'), 'OBJ export downloads geometry')
  const [glb] = await Promise.all([page.waitForEvent('download'), page.click('[data-action=exportGlb]')])
  const glbBytes = fs.readFileSync(await glb.path())
  check(glbBytes.subarray(0, 4).toString() === 'glTF', 'GLB export downloads binary glTF')
  await page.setInputFiles('#open-input', { name: 'scene.glb', mimeType: 'model/gltf-binary', buffer: glbBytes })
  await page.waitForFunction(() => document.querySelectorAll('#outliner li').length === 2)
  const imported = await faces()
  check(imported === 20, `GLB import welds and restores quads (10 + 10 faces, got ${imported})`)
  await page.click('[data-action=undo]')
  await page.waitForFunction(() => document.querySelectorAll('#outliner li').length === 1)

  await page.click('#outliner li')
  await page.click('[data-add-mod=array]')
  await page.waitForFunction(() => /30 faces/.test(document.querySelector('#status-mesh').textContent))
  await page.click('[data-add-mod=subdivision]')
  await page.waitForFunction(() => /120 faces/.test(document.querySelector('#status-mesh').textContent))
  check((await faces()) === 120, 'modifier stack evaluates array → subdivision')
  await page.click('[data-mod-apply]')
  await page.waitForFunction(() => !document.querySelector('[data-mod-apply]'))
  check((await faces()) === 120, 'apply bakes the stack into the base mesh')

  // Edit mode: Tab, face mode, click the cube's centre face, inset; then bevel all.
  await page.click('#outliner li')
  await page.keyboard.press('Delete')
  await page.waitForFunction(() => document.querySelectorAll('#outliner li').length === 0)
  await page.click('[data-add=cube]')
  await page.waitForFunction(() => /6 faces/.test(document.querySelector('#status-mesh').textContent))
  await settled()
  await page.keyboard.press('Tab')
  await page.waitForSelector('[data-mode=edit].on')
  await page.keyboard.press('3')
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2)
  await page.waitForFunction(() => /1 face selected/.test(document.querySelector('.face-info').textContent))
  await page.keyboard.press('a')
  await page.waitForFunction(() => /6 faces selected/.test(document.querySelector('.face-info').textContent))
  check(true, 'edit mode selects faces (click, select all)')
  await page.click('[data-action=inset]')
  await page.waitForFunction(() => /30 faces/.test(document.querySelector('#status-mesh').textContent))
  check((await faces()) === 30, 'inset acts on every selected face')
  await page.keyboard.press('Tab')
  await page.click('[data-action=undo]')
  await page.waitForFunction(() => /6 faces/.test(document.querySelector('#status-mesh').textContent))
  await page.click('[data-action=bevel]')
  await page.waitForFunction(() => /26 faces/.test(document.querySelector('#status-mesh').textContent))
  check((await faces()) === 26, 'bevel all chamfers the cube')

  await page.click('.tabs [data-tab=agent]')
  await page.click('#render-btn')
  await page.waitForFunction(() => document.querySelector('#render-out img')?.naturalWidth > 0)
  const rendered = await page.evaluate(() => document.querySelector('#render-out img').naturalWidth)
  check(rendered === 514, `agent view renders four tiles (${rendered}px wide)`)
  await page.click('.tabs [data-tab=properties]')

  // Materials: presets and sliders reach the engine; neon renders with bloom.
  await page.click('#outliner li')
  const material = () => page.evaluate(() => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].material))
  await page.click('[data-preset=glass]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].material.transmission === 1))
  await page.$eval('#p-opacity', (el) => {
    el.value = '0.5'
    el.dispatchEvent(new Event('change', { bubbles: true }))
  })
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].material.opacity === 0.5))
  check(true, 'glass preset and opacity slider edit the material')
  await page.click('[data-preset=neon]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].material.emissive_strength > 1))
  const neon = await material()
  check(neon.emissive === neon.color && neon.opacity === 1, `neon preset glows in its colour (${neon.emissive})`)
  // Textures: a pattern chip sets one; the viewport shows the baked tile.
  await page.click('[data-pattern=brick]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].material.texture?.pattern === 'brick'))
  await page.waitForFunction(() => window.__tatara.debug().textured === 1)
  await page.$eval('#p-tscale', (el) => {
    el.value = '1.5'
    el.dispatchEvent(new Event('change', { bubbles: true }))
  })
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].material.texture?.scale === 1.5))
  check(true, 'a pattern chip textures the object; the tile size slider rescales it')
  check((await page.evaluate(() => window.__tatara.debug().triplanar)) === 1, 'projected textures blend in the triplanar shader')
  await page.$eval('#p-relief', (el) => {
    el.value = '0.6'
    el.dispatchEvent(new Event('change', { bubbles: true }))
  })
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].material.texture?.relief === 0.6))
  await page.waitForFunction(() => window.__tatara.debug().normalMapped === 1)
  check(true, 'the relief slider adds a normal map')
  await page.setInputFiles('#image-input', { name: 'label.png', mimeType: 'image/png', buffer: TINY_PNG })
  await until(page, () =>
    fetch('/api/scene')
      .then((r) => r.json())
      .then((s) => s.images?.label && s.objects[0].material.texture?.image === 'label' && s.objects[0].material.texture.fit),
  )
  await page.waitForFunction(() => window.__tatara.debug().textured === 1)
  check(true, 'an uploaded image becomes a scene image and a fitted texture')
  check((await page.evaluate(() => window.__tatara.debug().triplanar)) === 0, 'fitted images use UVs, not the triplanar blend')
  // Node materials: the Nodes chip opens the editor; a preset wires
  // colour, roughness, metalness and height.
  await page.click('[data-pattern=nodes]')
  await page.waitForSelector('#node-editor:not([hidden]) .ne-node')
  await page.selectOption('#ne-preset', 'rust')
  await until(page, () =>
    fetch('/api/scene')
      .then((r) => r.json())
      .then((s) => s.objects[0].material.texture?.graph?.output?.metalness?.node === 'metal'),
  )
  await page.waitForFunction(() => window.__tatara.debug().roughnessMapped === 1)
  check(true, 'the node editor applies a graph that drives colour, roughness and metalness')
  await page.click('[data-ne-close]')
  await page.waitForSelector('#node-editor[hidden]', { state: 'attached' })
  await page.click('[data-pattern=none]')
  await page.waitForFunction(() => window.__tatara.debug().textured === 0)
  await page.waitForTimeout(300)

  // Rendered preview: the shading button path traces the view and keeps
  // refining it.
  await page.click('#shading-btn')
  await page.waitForFunction(() => window.__tatara.debug().pathSamples >= 2, null, { timeout: 60000 })
  check((await page.textContent('.view-label')).includes('Rendered'), 'the rendered preview path traces the view and refines it')
  check(await page.evaluate(() => getComputedStyle(document.querySelector('.pt-canvas')).display !== 'none'), 'the traced image covers the raster view')
  await page.click('#shading-btn')
  await page.waitForFunction(() => window.__tatara.debug().pathSamples === null)

  // Final render: the Render panel path traces the view at its own size.
  await page.click('#final-btn')
  await page.waitForSelector('#final-render:not([hidden])')
  await page.selectOption('#fr-size', '3')
  await page.selectOption('#fr-samples', '32')
  await page.check('#fr-dof')
  await page.click('[data-fr=render]')
  await page.waitForFunction(() => window.__tatara.debug().finalSamples >= 4, null, { timeout: 120000 })
  check((await page.$eval('.fr-canvas', (c) => `${c.width}x${c.height}`)) === '960x540', 'the Render panel path traces the view at the chosen size')
  await page.click('[data-fr=close]')
  await page.waitForSelector('#final-render[hidden]', { state: 'attached' })

  // Rigging: a bone chain from the Rig card; a Turn slider poses a bone.
  await page.click('[data-rig-chain="2"]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].bones?.length === 2))
  await page.waitForFunction(() => window.__tatara.debug().bonesShown === 1)
  await page.click('[data-bone="Bone 2"]')
  await page.waitForSelector('[data-bone="Bone 2"].on')
  await page.$eval('#p-bone-2', (el) => {
    el.value = '30'
    el.dispatchEvent(new Event('input', { bubbles: true }))
    el.dispatchEvent(new Event('change', { bubbles: true }))
  })
  await until(page, () =>
    fetch('/api/scene')
      .then((r) => r.json())
      .then((s) => Math.abs(s.objects[0].bones[1].rotation[2] - Math.PI / 6) < 1e-3),
  )
  check(true, 'the Rig card adds a bone chain and its sliders pose a bone')
  await page.click('[data-rig=ik]')
  await page.waitForFunction(() => window.__tatara.debug().reachHandle)
  check(true, 'Reach (IK) shows a handle at the chosen bone\'s tip')
  await page.click('[data-rig=ik]')
  await page.waitForFunction(() => !window.__tatara.debug().reachHandle)
  await page.click('[data-rig=remove]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => !s.objects[0].bones))
  await page.waitForFunction(() => window.__tatara.debug().bonesShown === 0)

  // UV editor: unwrap into islands, then move one island.
  await page.click('[data-uv-open]')
  await page.waitForSelector('#uv-editor:not([hidden]) .uv-canvas')
  await page.click('[data-unwrap=cube]')
  await page.waitForFunction(() => document.querySelector('#uv-hint')?.textContent.startsWith('6 islands'))
  check(true, 'unwrapping a cube lays out six islands')
  const uvBox = await page.locator('.uv-canvas').boundingBox()
  const uv0 = await page.evaluate(() =>
    fetch('/api/scene')
      .then((r) => r.json())
      .then((s) => s.objects[0].mesh.uvs[0]),
  )
  const [cu, cv] = uv0.reduce((a, p) => [a[0] + p[0] / uv0.length, a[1] + p[1] / uv0.length], [0, 0])
  await page.mouse.click(uvBox.x + cu * uvBox.width, uvBox.y + (1 - cv) * uvBox.height)
  await page.waitForFunction(() => document.querySelector('#uv-hint')?.textContent.startsWith('Island'))
  await page.click('[data-uv-op=grow]')
  await page.waitForFunction(() => document.querySelector('#activity li .what')?.textContent === 'transform_uvs')
  check(true, 'an island scales with one transform_uvs command')
  await page.click('[data-uv-close]')
  await page.waitForSelector('#uv-editor[hidden]', { state: 'attached' })

  // Sculpt mode: a drag on the object is one sculpt command, not an orbit.
  const before = await faces()
  await page.click('[data-action=subdivide]')
  await page.waitForFunction((n) => !document.querySelector('#status-mesh').textContent.includes(`${n} faces`), before.toLocaleString('en-US'))
  await page.click('[data-mode=sculpt]')
  await page.waitForSelector('[data-mode=sculpt].on')
  const orbit0 = await page.evaluate(() => JSON.stringify(window.__tatara.camera()))
  const rev0 = await page.evaluate(() => fetch('/api/scene').then((r) => r.json()).then((s) => s.revision))
  const cx = box.x + box.width / 2
  const cy = box.y + box.height / 2
  await page.mouse.move(cx - 40, cy)
  await page.mouse.down()
  for (let i = 1; i <= 8; i++) await page.mouse.move(cx - 40 + i * 10, cy - i * 2)
  await page.mouse.up()
  await until(page, (rev) => fetch('/api/scene').then((r) => r.json()).then((s) => s.revision === rev + 1), rev0)
  await page.waitForFunction(() => document.querySelector('#activity li .what')?.textContent === 'sculpt')
  check(true, 'dragging in sculpt mode sends one sculpt command')
  const orbit1 = await page.evaluate(() => JSON.stringify(window.__tatara.camera()))
  check(orbit0 === orbit1, 'the stroke does not orbit the camera')
  // Dynamic detail: the same drag now splits edges under the brush.
  const faces0 = await page.evaluate(() => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].mesh.faces.length))
  await page.click('[data-brush-toggle=dynamic]')
  await page.waitForSelector('#b-detail')
  await page.mouse.move(cx - 40, cy)
  await page.mouse.down()
  for (let i = 1; i <= 8; i++) await page.mouse.move(cx - 40 + i * 10, cy - i * 2)
  await page.mouse.up()
  await until(page, (rev) => fetch('/api/scene').then((r) => r.json()).then((s) => s.revision === rev + 2), rev0)
  const sent = await page.evaluate(() => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].mesh.faces.length))
  check(sent > faces0, `dynamic detail adds faces under the brush (${faces0} → ${sent})`)
  await page.click('[data-brush-toggle=dynamic]')
  await page.keyboard.press('Escape')
  await page.waitForSelector('[data-mode=object].on')

  // Boolean: cut a second cube out of the first from the Boolean card.
  await page.keyboard.press('Escape')
  await page.click('[data-add=cube]')
  await page.waitForFunction(() => document.querySelectorAll('#outliner li').length === 2)
  await page.click('#outliner li:nth-child(1)')
  await page.click('[data-bool=difference]')
  await page.waitForFunction(() => document.querySelector('#activity li .what')?.textContent === 'difference')
  check((await count()) === 1, 'boolean difference consumes the cutter')

  // World: a sky chip lights the view with the core's environment map;
  // the background checkbox shows it behind the scene.
  await page.keyboard.press('Escape')
  await page.click('[data-sky=sunset]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.world?.sky === 'sunset'))
  await page.waitForFunction(() => window.__tatara.debug().world === 'sky:sunset')
  await page.check('#w-bg')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.world?.background === true))
  await page.waitForFunction(() => window.__tatara.debug().worldBackground)
  check(true, 'a World sky lights the viewport and shows behind the scene')
  await page.click('[data-sky=studio]')
  await page.uncheck('#w-bg')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => !s.world))
  await page.waitForFunction(() => !window.__tatara.debug().worldBackground)

  // Proposals: changes offered for review show as cards; hovering one
  // previews it, Accept applies it and drops its sibling variant.
  const objects0 = await page.evaluate(() => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects.length))
  await page.evaluate(() =>
    fetch('/api/proposals', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ title: 'Lamps', variants: [
        { title: 'Round', commands: [{ op: 'add', name: 'Round lamp', primitive: { kind: 'sphere' }, translation: [3, 0.5, 0] }] },
        { title: 'Square', commands: [{ op: 'add', name: 'Square lamp', primitive: { kind: 'cube' }, translation: [3, 0.5, 0] }] },
      ] }),
    }),
  )
  await page.evaluate(() => window.__tatara.refresh(true))
  await page.waitForSelector('#proposals:not([hidden]) .proposal-card:nth-child(3)')
  await page.hover('#proposals .proposal-card:nth-child(3) .pc-title')
  await page.waitForFunction(() => window.__tatara.debug().previewing != null)
  check(true, 'a proposal card previews its change in the viewport')
  await page.click('#proposals .proposal-card:nth-child(3) [data-pc=accept]')
  await until(page, (n) => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects.length === n + 1 && s.objects.at(-1).name === 'Square lamp'), objects0)
  await page.waitForSelector('#proposals[hidden]', { state: 'attached' })
  check(true, 'accepting a variant applies it and drops the others')
  await page.evaluate(() => fetch('/api/undo', { method: 'POST' }))
  await page.evaluate(() => window.__tatara.refresh(true))

  // History: every batch is a step; revising one replays the rest.
  await page.click('.tabs [data-tab=history]')
  await page.waitForSelector('#history .hs-step')
  const total = await page.evaluate(() => fetch('/api/history').then((r) => r.json()).then((h) => h.total))
  check((await page.$$('#history .hs-step')).length === Math.min(total, 200), `the History tab lists every step (${total})`)
  await page.click('#history .hs-step:last-child .hs-head')
  await page.waitForSelector('#history .hs-step.open [data-hs=apply]')
  const revision0 = await page.evaluate(() => fetch('/api/scene').then((r) => r.json()).then((s) => s.revision))
  await page.click('#history [data-hs=apply]')
  await until(page, (r) => fetch('/api/scene').then((x) => x.json()).then((s) => s.revision === r + 1), revision0)
  check(true, 'a step re-applied from the History tab replays as one undo step')
  await page.click('.tabs [data-tab=properties]')

  // Assemblies: build a chair from the empty panel; it lists as one group.
  await page.click('[data-build=chair]')
  await page.waitForSelector('#outliner li[data-group="Chair"]')
  check((await page.textContent('#outliner li[data-group="Chair"] .count')) === '6', 'build makes a six-part chair group')
  await page.click('[data-group-act=delete]')
  await page.waitForFunction(() => !document.querySelector('#outliner li[data-group="Chair"]'))
  check(true, 'a group deletes as one')

  // Checks: the Inspect button shows the report; Drop settles the object.
  await page.click('.tabs [data-tab=agent]')
  await page.click('#inspect-btn')
  await page.waitForSelector('#checks-out .checks-summary')
  check(true, `inspect reports: ${(await page.textContent('#checks-out .checks-summary')).trim()}`)
  await page.click('.tabs [data-tab=properties]')
  await page.click('#outliner li')
  await page.click('[data-drop]')
  await page.waitForFunction(() => document.querySelector('#activity li .what')?.textContent === 'drop')
  check(true, 'drop settles the selected object')

  // Animation: key, scrub, auto-key an edit, play back.
  await page.click('#outliner li')
  await page.keyboard.press('k')
  await page.waitForFunction(() => document.querySelectorAll('#ruler-keys i').length === 1)
  await page.fill('#frame-input', '48')
  await page.press('#frame-input', 'Enter')
  await page.fill('[data-field=translation][data-i="0"]', '2')
  await page.press('[data-field=translation][data-i="0"]', 'Enter')
  await page.waitForFunction(() => document.querySelectorAll('#ruler-keys i').length === 2)
  check(true, 'K keys the pose; editing an animated value auto-keys it')
  const ctx = await page.evaluate(() => fetch('/api/context?frame=24').then((r) => r.json()))
  const x24 = ctx.objects[0].pose.transform.translation[0]
  check(x24 > 0 && x24 < 2, `pose is interpolated between keys (x=${x24.toFixed(3)} at frame 24)`)
  await page.fill('#frame-input', '1')
  await page.press('#frame-input', 'Enter')
  await page.locator('#viewport > canvas[data-engine]').focus()
  await page.keyboard.press('Space')
  await page.waitForTimeout(700)
  await page.keyboard.press('Space')
  const played = await page.evaluate(() => window.__tatara.frame())
  check(played > 1, `playback advances frames (stopped at ${played})`)

  await page.setViewportSize({ width: 390, height: 844 })
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)
  check(!overflow, 'phone layout has no horizontal scroll')
  check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join('; ')}` : ''}`)
  await page.close()

  // Every scenario must finish without errors (the MCP one falls back to HTTP).
  const cap = await browser.newPage({ viewport: { width: 1280, height: 720 } })
  await cap.goto(`${url}/?capture=1`)
  await cap.evaluate(() => window.__tatara.ready)
  for (const id of await cap.evaluate(() => window.__tatara.scenarios)) {
    await cap.reload()
    await cap.evaluate(() => window.__tatara.ready)
    await cap.evaluate((id) => window.__tatara.play(id), id)
    let st
    for (let i = 0; i < 1200; i++) {
      await cap.evaluate(() => window.__tatara.startTick(100))
      st = await (await cap.waitForFunction(() => window.__tatara.tickResult, null, { polling: 5 })).jsonValue()
      if (st.done) break
    }
    check(st.done && !st.error, `scenario ${id} completes${st.error ? `: ${st.error}` : ''}`)
  }
} finally {
  await browser.close()
  server.kill()
}
process.exit(failed ? 1 : 0)
