#!/usr/bin/env node
// Browser smoke test against an isolated editor: real clicks and keys for
// editing, history and files, then every demo scenario in capture mode.
// Requires `npm --prefix web run build` and `cargo build --release`.

import { spawn } from 'node:child_process'
import fs from 'node:fs'
import net from 'node:net'
import { createServer } from 'node:http'
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
// A real HTTP provider fixture exercises /api/chat without hosted models or keys.
const provider = createServer(async (req, res) => {
  let text = ''
  for await (const chunk of req) text += chunk
  const body = JSON.parse(text)
  const prompt = body.messages.at(-1).content
  const commands = prompt.includes('invalid') ? [{ op: 'delete', id: 999999 }] : [{ op: 'add', name: 'Chat cube', primitive: { kind: 'cube' }, color: '#c98268' }]
  res.writeHead(200, { 'Content-Type': 'application/json' })
  res.end(JSON.stringify({ choices: [{ message: { content: JSON.stringify({ commands }) } }] }))
})
await new Promise((resolve) => provider.listen(0, '127.0.0.1', resolve))
const providerUrl = `http://127.0.0.1:${provider.address().port}/v1`
const bin = path.join(root, 'target/release', process.platform === 'win32' ? 'tatara.exe' : 'tatara')
const startEditor = () => spawn(bin, [], { env: { ...process.env, TATARA_PORT: String(port), TATARA_WEB_DIR: path.join(root, 'web/dist'), TATARA_AI_BASE_URL: providerUrl, TATARA_AI_MODEL: 'browser-mock', TATARA_AI_API_KEY: '' }, stdio: 'inherit' })
let server = startEditor()
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
// Keep failures readable through the Checks API when CI log downloads are
// unavailable. Escape workflow-command data without changing the test outcome.
const annotateFailure = (message) => {
  if (process.env.GITHUB_ACTIONS !== 'true') return
  const escaped = String(message).replaceAll('%', '%25').replaceAll('\r', '%0D').replaceAll('\n', '%0A')
  console.log(`::error file=scripts/browser-check.mjs,title=Browser check failed::${escaped}`)
}
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`)
  if (!ok) {
    failed = true
    annotateFailure(what)
  }
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

  // Constraints: keep one box on another from the Constraints card; moving
  // the base carries the top along.
  await page.evaluate(() =>
    fetch('/api/commands', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ commands: [
        { op: 'add', name: 'Base', primitive: { kind: 'cube' }, translation: [6, 0.5, 0] },
        { op: 'add', name: 'Top', primitive: { kind: 'cube', size: 0.4 }, translation: [6, 1.2, 0] },
      ] }),
    }),
  )
  await page.evaluate(() => window.__tatara.refresh(true))
  await page.click('#outliner li:has-text("Top")')
  await page.waitForSelector('.constraints [data-constrain]')
  await page.selectOption('#c-target', { label: 'Base' })
  await page.click('[data-constrain]')
  await page.waitForSelector('.constraints .constraint-row')
  await page.evaluate(() => fetch('/api/commands', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ commands: [{ op: 'transform', id: 'Base', translation: [7, 0.5, 1] }] }) }))
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => Math.abs(s.objects.find((o) => o.name === 'Top').transform.translation[0] - 7) < 1e-6))
  check(true, 'a constraint from the Constraints card keeps the top on its base as the base moves')
  await page.evaluate(() => fetch('/api/commands', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ commands: [{ op: 'delete', id: 'Top' }, { op: 'delete', id: 'Base' }] }) }))
  await page.evaluate(() => window.__tatara.refresh(true))
  await page.keyboard.press('Escape')

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

  await page.click('.tabs [data-tab=agent]')
  check(await page.isChecked('#chat-review'), 'chat defaults to review before applying')
  const chatBefore = await page.evaluate(() => fetch('/api/scene').then((r) => r.json()))
  await page.fill('#chat-input', 'Add a cube for review')
  await page.click('#chat-send')
  await until(page, () => fetch('/api/proposals').then((r) => r.json()).then((p) => p.pending.some((x) => x.author === 'chat')))
  const chatAfter = await page.evaluate(() => fetch('/api/scene').then((r) => r.json()))
  check(chatAfter.revision === chatBefore.revision && JSON.stringify(chatAfter.objects) === JSON.stringify(chatBefore.objects), 'chat proposal leaves the authoritative scene unchanged')
  await page.waitForFunction(() => document.querySelector('#chat-status').textContent.startsWith('Ready to review'))
  check(await page.locator('#proposals .proposal-card').count() === 1, 'chat changes appear in the review tray')
  await page.hover('#proposals .proposal-card')
  await page.waitForFunction(() => window.__tatara.debug().previewing != null)
  check(true, 'chat proposal previews without applying')
  await page.click('#proposals [data-pc=accept]')
  await until(page, (n) => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects.length === n + 1), chatBefore.objects.length)
  check(true, 'accepting a chat proposal applies its batch')
  await page.click('[data-action=undo]')
  await until(page, (n) => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects.length === n), chatBefore.objects.length)
  check(true, 'a chat approval is one undo step')
  await page.fill('#chat-input', 'Another cube to reject')
  await page.click('#chat-send')
  await until(page, () => fetch('/api/proposals').then((r) => r.json()).then((p) => p.pending.length === 1))
  await page.click('#proposals [data-pc=reject]')
  await until(page, () => fetch('/api/proposals').then((r) => r.json()).then((p) => p.pending.length === 0))
  check((await count()) === chatBefore.objects.length, 'rejecting chat changes leaves the scene untouched')
  await page.uncheck('#chat-review')
  await page.fill('#chat-input', 'Add a cube directly')
  await page.click('#chat-send')
  await until(page, (n) => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects.length === n + 1), chatBefore.objects.length)
  check(true, 'chat can explicitly apply changes immediately')

  // Distance intent is entered through the same Constraints card as other rules.
  await page.evaluate(() => fetch('/api/commands', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({commands: [
    {op: 'clear'}, {op: 'add', name: 'Anchor', primitive: {kind: 'cube'}, translation: [-1, 0.5, 0]},
    {op: 'add', name: 'Tether', primitive: {kind: 'cube'}, translation: [1, 0.5, 0]},
  ]}) }).then((r) => r.json()))
  await until(page, () => document.querySelectorAll('#outliner li').length === 2)
  await page.click('.tabs [data-tab=properties]')
  await page.click('#outliner li:nth-child(2)')
  await page.selectOption('#c-kind', 'distance')
  await page.fill('#c-distance', '0.125')
  await page.click('[data-constrain]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => (s.constraints || []).some((c) => c.kind === 'distance' && c.distance === 0.125)))
  await page.waitForFunction(() => document.querySelector('.constraints').textContent.includes('0.125 m from Anchor'))
  check(true, 'the distance UI accepts arbitrary fractional metres')
  await page.fill('#c-distance', '2')
  await page.click('[data-constrain]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => (s.constraints || []).some((c) => c.kind === 'distance' && c.distance === 2)))
  check((await page.textContent('.constraints')).includes('2 m from Anchor'), 'the Constraints card creates and describes a distance rule')
  await page.evaluate(() => fetch('/api/commands', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({commands: [{op: 'transform', id: 'Anchor', translation: [-2, 0.5, 0]}]})}))
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[1].transform.translation[0] === 0))
  check(true, 'moving the reference keeps the specified centre distance')
  await page.evaluate(() => fetch('/api/commands', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({commands: [{op: 'transform', id: 'Tether', translation: [2, 0.5, 0]}]})}))
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].transform.translation[0] === 0))
  check(true, 'moving the constrained side leads its distance partner')
  await page.click('[data-action=undo]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].transform.translation[0] === -2 && s.objects[1].transform.translation[0] === 0))
  check(true, 'one Undo restores both distance endpoints')
  await page.click('.constraints [data-unconstrain]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => (s.constraints || []).length === 0))
  check(true, 'the distance rule can be removed from the Constraints card')

  // Axis alignment leaves the other coordinates free, whichever endpoint leads.
  await page.evaluate(() => fetch('/api/commands', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({commands: [
    {op: 'clear'}, {op: 'add', name: 'Anchor', primitive: {kind: 'cube'}, translation: [-2,1,0]},
    {op: 'add', name: 'Partner', primitive: {kind: 'cube'}, translation: [2,3,4]},
  ]})}))
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects.length === 2 && s.objects[1].name === 'Partner' && s.objects[1].transform.translation[1] === 3))
  await page.click('#outliner li:last-child')
  await page.selectOption('#c-kind', 'align')
  await page.selectOption('#c-align', 'y')
  await page.click('[data-constrain]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => (s.constraints || []).some((c) => c.kind === 'align' && c.axes === 'y') && s.objects[1].transform.translation[1] === 1))
  check((await page.textContent('.constraints')).includes('Aligned Y with Anchor'), 'the Constraints card creates and describes world-axis alignment')
  await page.evaluate(() => fetch('/api/commands', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({commands: [{op: 'transform', id: 'Anchor', translation: [-3,2,1]}]})}))
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => JSON.stringify(s.objects[1].transform.translation) === '[2,2,4]'))
  check(true, 'alignment follows the anchor while unselected axes stay free')
  await page.evaluate(() => fetch('/api/commands', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({commands: [{op: 'transform', id: 'Partner', translation: [5,4,6]}]})}))
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => JSON.stringify(s.objects[0].transform.translation) === '[-3,4,1]'))
  check(true, 'either alignment endpoint can lead')
  await page.click('[data-action=undo]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].transform.translation[1] === 2 && s.objects[1].transform.translation[1] === 2))
  check(true, 'one Undo restores both alignment endpoints')
  await page.selectOption('#c-align', 'xz')
  await page.click('[data-constrain]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => (s.constraints || []).length === 1 && s.constraints[0].axes === 'xz' && JSON.stringify(s.objects[1].transform.translation) === '[-3,2,1]'))
  check(true, 'multiple alignment axes replace the existing pair rule')
  await page.click('.constraints [data-unconstrain]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => (s.constraints || []).length === 0))
  check(true, 'alignment can be removed from the Constraints card')

  // A maintained arrangement follows its reference and is reviewable intent.
  await page.evaluate(() => fetch('/api/commands', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({commands: [
    {op: 'clear'}, {op: 'add', name: 'Anchor', primitive: {kind: 'cube'}, scale: [0.4,0.4,0.4], translation: [0,0.2,0]},
    {op: 'add', name: 'A', primitive: {kind: 'cube'}, scale: [0.4,0.4,0.4], translation: [-2,0.2,0]},
    {op: 'add', name: 'B', primitive: {kind: 'cube'}, scale: [0.4,0.4,0.4], translation: [2,0.2,0]},
  ]})}))
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects.length === 3 && s.objects[0].name === 'Anchor'))
  await page.click('#outliner li:first-child')
  await page.click('.arrangement-create summary')
  await page.selectOption('#layout-kind', 'row')
  await page.fill('#layout-spacing', '0.8')
  await page.click('[data-keep-layout]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => (s.arrangements || []).length === 1 && Math.abs(s.objects[1].transform.translation[0] + 0.6) < 1e-6))
  check((await page.textContent('.arrangements')).includes('row · 2 items · around Anchor'), 'the Arrangements card creates a maintained row')
  await page.evaluate(() => fetch('/api/commands', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({commands: [{op: 'transform', id: 'Anchor', translation: [1,0.2,1]}]})}))
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => Math.abs(s.objects[1].transform.translation[0] - 0.4) < 1e-6 && Math.abs(s.objects[2].transform.translation[0] - 1.6) < 1e-6 && s.objects[2].transform.translation[2] === 1))
  check(true, 'maintained layout items follow the reference')
  await page.click('[data-action=undo]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].transform.translation[0] === 0 && Math.abs(s.objects[1].transform.translation[0] + 0.6) < 1e-6 && s.objects[2].transform.translation[2] === 0))
  check(true, 'one Undo restores the reference and its entire layout')
  await page.evaluate(() => fetch('/api/commands', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({commands: [{op: 'transform', id: 'A', translation: [9,0.2,9]}]})}))
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => Math.abs(s.objects[1].transform.translation[0] + 0.6) < 1e-6 && s.objects[1].transform.translation[2] === 0))
  check(true, 'individual member moves snap back to the maintained layout')
  await page.click('.arrangement-create summary')
  await page.selectOption('#layout-kind', 'circle')
  await page.fill('#layout-radius', '1')
  await page.click('[data-keep-layout]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => (s.arrangements || []).length === 1 && s.arrangements[0].layout === 'circle' && Math.abs(s.objects[1].transform.translation[2] - 1) < 1e-6))
  check(true, 'the same member set can replace a row with a maintained circle')
  await page.evaluate(() => fetch('/api/commands', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({commands: [{op: 'delete', id: 'B'}]})}))
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects.length === 2 && s.arrangements[0].items.length === 1))
  check(true, 'deleting a member updates the maintained layout')
  await page.click('[data-unarrange]')
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => (s.arrangements || []).length === 0))
  await page.evaluate(() => fetch('/api/commands', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({commands: [{op: 'transform', id: 'A', translation: [9,0.2,9]}]})}))
  await until(page, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[1].transform.translation[0] === 9))
  check(true, 'removing a maintained arrangement releases its members')
  await page.evaluate(() => fetch('/api/proposals', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({title: 'Keep a row', commands: [{op: 'arrange', ids: ['A'], layout: 'row', around: 'Anchor', keep: true}]})}))
  await page.waitForFunction(() => document.querySelector('#proposals').textContent.includes('arrangements'))
  check((await page.evaluate(() => fetch('/api/scene').then((r) => r.json()))).arrangements == null, 'proposal cards describe arrangement intent before it is applied')
  await page.click('#proposals [data-pc=reject]')

  await page.setViewportSize({ width: 390, height: 844 })
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)
  check(!overflow, 'phone layout has no horizontal scroll')
  check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join('; ')}` : ''}`)
  await page.close()

  // Two independent browsers share authoritative edits and advisory presence.
  const alice = await browser.newPage({ viewport: { width: 1280, height: 720 } })
  const bob = await browser.newPage({ viewport: { width: 1280, height: 720 } })
  await Promise.all([alice.goto(url), bob.goto(url)])
  await Promise.all([alice.evaluate(() => window.__tatara.ready), bob.evaluate(() => window.__tatara.ready)])
  await alice.fill('#session-name', 'Alice')
  await alice.press('#session-name', 'Tab')
  await bob.fill('#session-name', 'Bob')
  await bob.press('#session-name', 'Tab')
  await until(alice, () => window.__tatara.presence().some((p) => p.actor.name === 'Bob'))
  await until(bob, () => window.__tatara.presence().some((p) => p.actor.name === 'Alice'))
  check(true, 'two browser sessions see each other')
  const aBox = await alice.locator('#viewport').boundingBox()
  await alice.mouse.move(aBox.x + aBox.width * 0.55, aBox.y + aBox.height * 0.4)
  await bob.waitForFunction(() => [...document.querySelectorAll('.peer-cursor')].some((e) => e.textContent.includes('Alice')))
  check(true, 'a remote cursor is visible')
  await alice.click('[data-add=cube]')
  await alice.waitForFunction(() => window.__tatara.selection().id != null)
  const sharedId = await alice.evaluate(() => window.__tatara.selection().id)
  await until(bob, (id) => Boolean(document.querySelector(`#outliner [data-id="${id}"]`)), sharedId)
  await bob.click(`#outliner [data-id="${sharedId}"]`)
  await alice.waitForFunction((id) => document.querySelector(`#outliner [data-id="${id}"]`).classList.contains('peer-selected'), sharedId)
  check(true, 'remote selections are marked in the outliner and viewport')
  await bob.locator('#p-name').focus()
  await alice.waitForFunction((id) => document.querySelector(`#outliner [data-id="${id}"]`).classList.contains('peer-editing'), sharedId)
  check(true, 'focused properties announce an advisory editing lock')
  await bob.fill('#p-name', 'Bob cube')
  await bob.locator('#p-name').press('Tab')
  await until(alice, (id) => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects.find((o) => o.id === id)?.name === 'Bob cube'), sharedId)
  check(true, 'edits from the second browser appear in the first')
  await bob.click('#outliner')
  await alice.waitForFunction((id) => !document.querySelector(`#outliner [data-id="${id}"]`).classList.contains('peer-editing'), sharedId)
  check(true, 'leaving an edit releases the advisory lock')
  await bob.bringToFront()
  const bBox = await bob.locator('#viewport').boundingBox()
  await bob.mouse.move(bBox.x + bBox.width * 0.8, bBox.y + bBox.height * 0.7)
  await bob.mouse.down()
  await bob.mouse.move(bBox.x + bBox.width * 0.7, bBox.y + bBox.height * 0.7, { steps: 5 })
  await bob.mouse.up()
  await bob.waitForFunction(() => {
    const orbit = window.__tatara.camera()
    const old = window.__collabCameraWait
    const stable = old && Math.abs(old.azimuth - orbit.azimuth) < 0.01
    window.__collabCameraWait = { azimuth: orbit.azimuth, count: stable ? old.count + 1 : 0 }
    return window.__collabCameraWait.count >= 5
  }, null, { polling: 'raf', timeout: 60000 })
  const bobOrbit = await bob.evaluate(() => window.__tatara.camera())
  await until(alice, (desired) => {
    const c = window.__tatara.presence().find((p) => p.actor.name === 'Bob')?.camera
    if (!c) return false
    const az = Math.atan2(c.eye[0] - c.target[0], c.eye[2] - c.target[2]) * 180 / Math.PI
    return Math.abs(((az - desired + 540) % 360) - 180) < 0.5
  }, bobOrbit.azimuth)
  const cameraPeer = await alice.evaluate(() => window.__tatara.presence().find((p) => p.actor.name === 'Bob'))
  check(Boolean(cameraPeer.camera?.eye?.length === 3), 'remote camera presence is available')
  const cameraButton = await alice.locator('.session-peer').filter({ hasText: 'Bob' }).getByRole('button', { name: 'View camera' }).elementHandle()
  await bob.mouse.move(bBox.x + bBox.width * 0.3, bBox.y + bBox.height * 0.3)
  await until(alice, () => {
    const cursor = window.__tatara.presence().find((p) => p.actor.name === 'Bob')?.cursor
    return cursor && Math.abs(cursor[0] - 0.3) < 0.01
  })
  check(await cameraButton.evaluate((button) => button.isConnected), 'presence updates preserve peer camera controls')
  await alice.bringToFront()
  await alice.locator('.session-peer').filter({ hasText: 'Bob' }).getByRole('button', { name: 'View camera' }).click()
  const desired = Math.atan2(cameraPeer.camera.eye[0] - cameraPeer.camera.target[0], cameraPeer.camera.eye[2] - cameraPeer.camera.target[2]) * 180 / Math.PI
  await alice.waitForFunction((desired) => Math.abs(((window.__tatara.camera().azimuth - desired + 540) % 360) - 180) < 1, desired)
  check(true, 'a participant can view a peer camera')
  const conflict = await alice.evaluate(async (id) => {
    const revision = (await (await fetch('/api/scene')).json()).revision
    const actor = window.__tatara.actor()
    const responses = await Promise.all(['Alice edit', 'Concurrent edit'].map((name) => fetch('/api/commands', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ expected_revision: revision, actor, commands: [{ op: 'rename', id, name }] }) })))
    return Promise.all(responses.map(async (r) => ({ status: r.status, data: await r.json() })))
  }, sharedId)
  check(conflict.map((r) => r.status).sort().join(',') === '200,409' && conflict.find((r) => r.status === 409).data.error.includes('refresh the scene'), 'simultaneous stale edits are rejected with a recovery reason')
  await until(bob, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.revision === Number(document.querySelector('#status-rev').textContent.replace('Revision ', ''))))
  const attribution = await alice.evaluate(() => fetch('/api/history').then((r) => r.json()).then((h) => h.steps.at(-1).actor.name))
  check(attribution === 'Alice', 'shared edits retain their author in history')
  // Real UI edits from two browsers share an older context without losing either.
  await alice.evaluate(() => fetch('/api/commands', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({commands: [
    {op: 'clear'}, {op: 'add', name: 'A', primitive: {kind: 'cube'}},
    {op: 'add', name: 'B', primitive: {kind: 'cube'}, translation: [3,0,0]},
  ]})}))
  await Promise.all([alice, bob].map((p) => until(p, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects.length === 2 && s.objects[0].name === 'A' && s.objects[1].name === 'B'))))
  const context = await alice.evaluate(() => fetch('/api/scene').then((r) => r.json()).then((s) => s.revision))
  await alice.click('#outliner li:first-child')
  await bob.click('#outliner li:last-child')
  await alice.fill('#p-name', 'A combined')
  await bob.fill('#p-name', 'B first')
  await bob.locator('#p-name').press('Tab')
  await until(bob, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[1].name === 'B first'))
  await bob.click('#outliner')
  await alice.locator('#p-name').press('Tab')
  await until(alice, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].name === 'A combined' && s.objects[1].name === 'B first'))
  await until(bob, () => document.querySelector('#outliner').textContent.includes('A combined'))
  const combined = await alice.evaluate(() => fetch('/api/history').then((r) => r.json()).then((h) => h.steps.at(-1)))
  check(combined.rebased_from === context && combined.actor.name === 'Alice', 'two browsers preserve independent real UI edits from the same context')
  await alice.click('.tabs [data-tab=history]')
  await alice.waitForFunction(() => document.querySelector('#history').textContent.includes('combined'))
  check(true, 'history marks an edit combined with newer changes')
  await alice.click('[data-action=undo]')
  await until(alice, () => fetch('/api/scene').then((r) => r.json()).then((s) => s.objects[0].name === 'A' && s.objects[1].name === 'B first'))
  await until(bob, () => document.querySelector('#outliner').textContent.includes('B first') && !document.querySelector('#outliner').textContent.includes('A combined'))
  check(true, 'Undo removes only the rebased edit and keeps the other browser change')
  const rebasedConflict = await alice.evaluate(async () => {
    const s = await (await fetch('/api/scene')).json()
    const responses = await Promise.all(['First', 'Second'].map((name) => fetch('/api/commands', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({expected_revision: s.revision, rebase: true, commands: [{op: 'rename', id: s.objects[0].id, name}]})})))
    return Promise.all(responses.map(async (r) => ({status: r.status, data: await r.json()})))
  })
  check(rebasedConflict.map((r) => r.status).sort().join(',') === '200,409' && rebasedConflict.find((r) => r.status === 409).data.error.includes('cannot rebase'), 'same-object concurrent edits are refused even when rebase is enabled')
  // A new server starts at revision/presence version zero. Open tabs must
  // recover from that, not confuse it with an old response from this session.
  const exited = new Promise((resolve) => server.once('exit', resolve))
  server.kill()
  await exited
  server = startEditor()
  await until(alice, () => document.querySelector('#status-rev').textContent === 'Revision 0')
  await until(bob, () => document.querySelector('#status-rev').textContent === 'Revision 0')
  check((await alice.locator('#outliner li').count()) === 0 && (await bob.locator('#outliner li').count()) === 0, 'open browsers recover the empty scene after a server restart')
  await until(alice, () => window.__tatara.presence().some((p) => p.actor.name === 'Bob'))
  await until(bob, () => window.__tatara.presence().some((p) => p.actor.name === 'Alice'))
  check(true, 'presence rejoins after a server restart with a new version counter')
  await alice.close()
  // A browser process may omit pagehide when closing a tab. Its 30-second
  // lease still expires; allow the peer's next heartbeat to publish expiry.
  await bob.waitForFunction(() => !window.__tatara.presence().some((p) => p.actor.name === 'Alice'), null, { timeout: 45000 })
  check(true, 'closing a browser removes its presence')
  await bob.close()


  // A real UI scene camera shares commands, lens edits, animation and export.
  const shot = await browser.newPage({viewport:{width:1280,height:720},acceptDownloads:true})
  await shot.goto(url)
  await shot.evaluate(() => window.__tatara.ready)
  await shot.click('[data-action=addCamera]')
  await until(shot, () => document.querySelector('#camera-fov'))
  const cameraScene = await shot.evaluate(() => fetch('/api/scene').then(r => r.json()))
  const cameraId = cameraScene.objects.find(o => o.camera).id
  check(cameraScene.objects.find(o => o.camera).mesh.faces.length === 0, 'the Camera button creates a non-rendering scene camera from the current view')
  await shot.fill('#camera-fov', '52')
  await shot.locator('#camera-fov').blur()
  await until(shot, () => document.querySelector('#camera-fov')?.value === '52')
  await shot.click('[data-look-camera]')
  await until(shot, id => window.__tatara.debug().sceneCamera === id && window.__tatara.debug().renderCamera.fov === 52, cameraId)
  check(true, 'Look through uses the stored lens and disables orbit changes')
  const cameraRender = await shot.evaluate(async id => {
    const r = await fetch(`/api/render/image?camera=${id}&w=8&h=8&samples=1`)
    return {status:r.status,bytes:Array.from(new Uint8Array(await r.arrayBuffer()).slice(0,4))}
  }, cameraId)
  check(cameraRender.status === 200 && cameraRender.bytes.join(',') === '137,80,78,71', 'the scene camera renders a PNG through the shared Rust core')
  await shot.click('[data-action=undo]')
  await until(shot, () => window.__tatara.debug().renderCamera.fov === 36)
  check(true, 'Undo restores the camera lens while looking through it')
  await shot.click('[data-exit-camera]')
  await until(shot, () => window.__tatara.debug().sceneCamera == null)
  await shot.click('[data-look-camera]')
  await shot.keyboard.press('Escape')
  await until(shot, () => window.__tatara.debug().sceneCamera == null)
  check(true, 'Escape returns from the scene camera to the saved orbit')
  const [shotGlb] = await Promise.all([shot.waitForEvent('download'),shot.click('[data-action=exportGlb]')])
  const shotBytes=fs.readFileSync(await shotGlb.path())
  const docLen=shotBytes.readUInt32LE(12)
  const shotDoc=JSON.parse(shotBytes.subarray(20,20+docLen).toString())
  check(shotDoc.cameras?.length === 1 && shotDoc.nodes.some(n => n.camera === 0), 'a camera-only scene exports a valid camera node in GLB')
  await shot.click('[data-action=delete]')
  await until(shot, () => document.querySelectorAll('#outliner li').length === 0)
  check(true, 'the camera can be removed with the ordinary Delete command')

  await shot.click('[data-action=addLight]')
  await until(shot, () => document.querySelector('#light-intensity') && window.__tatara.debug().sceneLights === 1)
  check(true, 'the Light button creates an editable point light and a real viewport light')
  await shot.fill('#light-intensity','35')
  await shot.locator('#light-intensity').blur()
  await until(shot, () => document.querySelector('#light-intensity')?.value === '35' && window.__tatara.debug().pending === 0)
  const litScene=await shot.evaluate(() => fetch('/api/scene').then(r=>r.json()))
  check(litScene.objects[0].light.intensity===35,'the light inspector sends one typed settings command')
  await shot.selectOption('#light-kind','sun')
  await until(shot, () => document.querySelector('#light-kind')?.value === 'sun' && window.__tatara.debug().pending === 0)
  const sunScene=await shot.evaluate(() => fetch('/api/scene').then(r=>r.json()))
  check(sunScene.objects[0].light.kind === 'sun' && sunScene.objects[0].light.intensity === 35 && sunScene.objects[0].light.color === '#ffffff','switching to a sun light keeps its saved power and colour')
  await shot.click('[data-action=undo]')
  await until(shot, () => document.querySelector('#light-kind')?.value === 'point')
  check(true,'Undo restores the light type in the scene and viewport')
  await shot.fill('#light-intensity','0')
  await shot.locator('#light-intensity').blur()
  await until(shot, () => window.__tatara.debug().sceneLights === 0)
  check(true,'zero intensity switches a scene light off without deleting it')
  const [lightGlb]=await Promise.all([shot.waitForEvent('download'),shot.click('[data-action=exportGlb]')])
  const lightBytes=fs.readFileSync(await lightGlb.path())
  const lightDoc=JSON.parse(lightBytes.subarray(20,20+lightBytes.readUInt32LE(12)).toString())
  check(lightDoc.extensions?.KHR_lights_punctual?.lights?.[0]?.type === 'point','scene lights export through the standard glTF punctual-light extension')
  await shot.close()

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
} catch (error) {
  annotateFailure(error.stack ?? error)
  throw error
} finally {
  await browser.close()
  server.kill()
  provider.close()
}
process.exit(failed ? 1 : 0)
