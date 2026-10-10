#!/usr/bin/env node
// Smoke test for the browser-only build (web/dist-static): serves it under a
// sub-path like GitHub Pages and drives it in Chromium with no server API.
// Run `node scripts/build-static.mjs` first.

import fs from 'node:fs'
import http from 'node:http'
import path from 'node:path'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const dist = path.join(root, 'web/dist-static')
const { chromium } = createRequire(path.join(root, 'web/package.json'))('playwright-core')
const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.wasm': 'application/wasm' }

let apiHits = 0
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://x')
  if (url.pathname.includes('/api/')) apiHits++
  const rel = url.pathname.replace(/^\/Tatara\/?/, '') || 'index.html'
  const file = path.join(dist, rel)
  if (!file.startsWith(dist) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
    res.writeHead(404).end()
    return
  }
  res.writeHead(200, { 'Content-Type': types[path.extname(file)] || 'application/octet-stream' })
  fs.createReadStream(file).pipe(res)
})
await new Promise((r) => server.listen(0, '127.0.0.1', r))
const base = `http://127.0.0.1:${server.address().port}/Tatara/`

function findBrowser() {
  if (process.env.TATARA_BROWSER_PATH) return process.env.TATARA_BROWSER_PATH
  const dir = '/opt/pw-browsers'
  if (!fs.existsSync(dir)) return undefined
  for (const d of fs.readdirSync(dir).sort().reverse()) {
    const exe = path.join(dir, d, 'chrome-linux/chrome')
    if (d.startsWith('chromium-') && fs.existsSync(exe)) return exe
  }
}

let failed = false
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`)
  if (!ok) failed = true
}

const browser = await chromium.launch({ executablePath: findBrowser(), args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] })
try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 720 }, acceptDownloads: true })
  const errors = []
  page.on('pageerror', (e) => errors.push(e.message))
  await page.goto(base)
  await page.evaluate(() => window.__tatara.ready)
  check(await page.isVisible('.wasm-badge'), 'browser-only badge is shown')
  const faces = async () => Number((await page.textContent('#status-mesh')).match(/([\d,]+) faces/)[1].replace(/,/g, ''))

  await page.click('[data-add=cube]')
  await page.waitForFunction(() => document.querySelectorAll('#outliner li').length === 1)
  await page.click('[data-add-mod=array]')
  await page.waitForFunction(() => /18 faces/.test(document.querySelector('#status-mesh').textContent))
  check((await faces()) === 18, 'WebAssembly core evaluates a modifier stack')
  await page.click('[data-action=undo]')
  await page.waitForFunction(() => /6 faces/.test(document.querySelector('#status-mesh').textContent))
  check(true, 'undo works without a server')
  await page.click('#outliner li')
  await page.click('[data-pattern=wood]')
  await page.waitForFunction(() => window.__tatara.debug().textured === 1)
  await page.$eval('#p-relief', (el) => {
    el.value = '0.5'
    el.dispatchEvent(new Event('change', { bubbles: true }))
  })
  await page.waitForFunction(() => window.__tatara.debug().normalMapped === 1)
  check(true, 'textures and relief normal maps are baked by the WebAssembly core')

  const [glb] = await Promise.all([page.waitForEvent('download'), page.click('[data-action=exportGlb]')])
  const bytes = fs.readFileSync(await glb.path())
  check(bytes.subarray(0, 4).toString() === 'glTF', 'GLB export downloads from WebAssembly')
  await page.setInputFiles('#open-input', { name: 'scene.glb', mimeType: 'model/gltf-binary', buffer: bytes })
  await page.waitForFunction(() => document.querySelectorAll('#outliner li').length === 2)
  check(true, 'GLB import runs in WebAssembly')

  await page.click('.tabs [data-tab=agent]')
  await page.click('#render-btn')
  await page.waitForFunction(() => document.querySelector('#render-out img')?.naturalWidth > 0)
  check((await page.evaluate(() => document.querySelector('#render-out img').naturalWidth)) === 514, 'software renderer runs in WebAssembly')
  await page.click('#shading-btn')
  await page.waitForFunction(() => window.__tatara.debug().pathSamples >= 1, null, { timeout: 90000 })
  check(true, 'the path tracer runs in WebAssembly')
  await page.click('#shading-btn')
  await page.click('.tabs [data-tab=properties]')

  await page.click('#demo-btn')
  // The tour ends with a plinth, the sculpted head and the agent's three objects.
  await page.waitForFunction(() => document.querySelectorAll('#outliner li').length === 5, null, { timeout: 90000 })
  await page.waitForFunction(() => !document.querySelector('#demo-btn').disabled, null, { timeout: 90000 })
  check(true, 'the tour plays')
  const distance = await page.evaluate(async () => {
    const post = (path, commands) => fetch(`/api/${path}`, {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({commands})}).then((r) => r.json())
    await post('commands', [{op: 'clear'}, {op: 'add', name: 'A', primitive: {kind: 'cube'}}, {op: 'add', name: 'B', primitive: {kind: 'cube'}, translation: [3,0,0]}, {op: 'constrain', id: 'B', distance: 2, from: 'A'}])
    await post('commands', [{op: 'transform', id: 'A', translation: [1,0,0]}])
    const moved = await (await fetch('/api/scene')).json()
    await post('undo', [])
    const undone = await (await fetch('/api/scene')).json()
    return {moved, undone}
  })
  check(distance.moved.objects[0].transform.translation[0] === 1 && distance.moved.objects[1].transform.translation[0] === 3, 'distance intent is solved by the WebAssembly core')
  check(distance.undone.objects[0].transform.translation[0] === 0 && distance.undone.objects[1].transform.translation[0] === 2, 'distance endpoint changes are one Undo in WebAssembly')
  const aligned = await page.evaluate(async () => {
    const post = (path, commands) => fetch(`/api/${path}`, {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({commands})}).then((r) => r.json())
    await post('commands', [{op: 'clear'}, {op: 'add', name: 'A', primitive: {kind: 'cube'}, translation: [0,1,0]}, {op: 'add', name: 'B', primitive: {kind: 'cube'}, translation: [3,2,4]}, {op: 'constrain', id: 'B', align: 'y', from: 'A'}])
    await post('commands', [{op: 'transform', id: 'B', translation: [5,6,7]}])
    const moved = await (await fetch('/api/scene')).json()
    await post('undo', [])
    const undone = await (await fetch('/api/scene')).json()
    return {moved, undone}
  })
  check(JSON.stringify(aligned.moved.objects[0].transform.translation) === '[0,6,0]' && JSON.stringify(aligned.moved.objects[1].transform.translation) === '[5,6,7]', 'WebAssembly alignment preserves free axes and follows either endpoint')
  check(JSON.stringify(aligned.undone.objects[0].transform.translation) === '[0,1,0]' && JSON.stringify(aligned.undone.objects[1].transform.translation) === '[3,1,4]', 'WebAssembly alignment edits are one Undo')
  const layout = await page.evaluate(async () => {
    const post = (path, commands) => fetch(`/api/${path}`, {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({commands})}).then((r) => r.json())
    await post('commands', [{op: 'clear'}, {op: 'add', name: 'Anchor', primitive: {kind: 'cube'}}, {op: 'add', name: 'A', primitive: {kind: 'cube'}, translation: [-3,0,0]}, {op: 'add', name: 'B', primitive: {kind: 'cube'}, translation: [3,0,0]}, {op: 'arrange', ids: ['A','B'], layout: 'row', around: 'Anchor', spacing: 2, keep: true}])
    await post('commands', [{op: 'move', id: 'Anchor', offset: [2,0,3]}])
    const moved = await (await fetch('/api/scene')).json()
    await post('undo', [])
    const undone = await (await fetch('/api/scene')).json()
    return {moved, undone}
  })
  check(JSON.stringify(layout.moved.objects[1].transform.translation) === '[0.5,0.5,3]' && JSON.stringify(layout.moved.objects[2].transform.translation) === '[3.5,0.5,3]', 'maintained layouts follow their reference in WebAssembly')
  check(JSON.stringify(layout.undone.objects[1].transform.translation) === '[-1.5,0.5,0]' && layout.undone.arrangements.length === 1, 'WebAssembly keeps layout intent through Undo')
  const creations = await page.evaluate(async () => {
    const post = body => fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)}).then(r=>r.json())
    const base = await post({commands:[{op:'clear'},{op:'add_camera',name:'First shot'}]})
    const first = await post({commands:[{op:'add_light',name:'First key'}]})
    const second = await post({expected_revision:base.revision,rebase:true,commands:[{op:'add_camera',name:'Second shot'}]})
    const rejected = await fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({expected_revision:base.revision,rebase:true,commands:[{op:'add_light',name:'First key'}]})})
    return {base,first,second,status:rejected.status}
  })
  check(creations.second.rebased_from === creations.base.revision && creations.second.created[0] > creations.first.created[0], 'WebAssembly rebases camera creations using current IDs')
  check(creations.status === 409, 'WebAssembly rejects stale creation name collisions')
  const rebased = await page.evaluate(async () => {
    const post = (path, body = {}) => fetch(`/api/${path}`, {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify(body)}).then((r) => r.json())
    const setup = await post('commands', {commands: [{op: 'clear'}, {op: 'add', name: 'A', primitive: {kind: 'cube'}}, {op: 'add', name: 'B', primitive: {kind: 'cube'}, translation: [3,0,0]}]})
    await post('commands', {commands: [{op: 'transform', id: 'A', translation: [1,0,0]}], expected_revision: setup.revision})
    const result = await post('commands', {commands: [{op: 'material', id: 'B', color: '#ff0000'}], expected_revision: setup.revision, rebase: true})
    const moved = await (await fetch('/api/scene')).json()
    await post('undo')
    const undone = await (await fetch('/api/scene')).json()
    return {context: setup.revision, result, moved, undone}
  })
  check(rebased.result.rebased_from === rebased.context && rebased.moved.objects[0].transform.translation[0] === 1 && rebased.moved.objects[1].material.color === '#ff0000', 'WebAssembly rebases independent edits from the same context')
  check(rebased.undone.objects[0].transform.translation[0] === 1 && rebased.undone.objects[1].material.color !== '#ff0000', 'Undo preserves the earlier edit after a WebAssembly rebase')

  const sceneCamera = await page.evaluate(async () => {
    const post = commands => fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands})}).then(r=>r.json())
    await post([{op:'clear'},{op:'add_camera',name:'Static shot',translation:[0,1,4],lens:{fov:50,focus:4}}])
    const before=await (await fetch('/api/scene')).json()
    const image=await fetch('/api/render/image?camera=Static%20shot&w=8&h=8&samples=1')
    const bytes=Array.from(new Uint8Array(await image.arrayBuffer()).slice(0,4))
    await post([{op:'camera_settings',id:'Static shot',lens:{fov:60}}])
    await fetch('/api/undo',{method:'POST'})
    const after=await (await fetch('/api/scene')).json()
    return {before,after,status:image.status,bytes}
  })
  check(sceneCamera.status === 200 && sceneCamera.bytes.join(',') === '137,80,78,71', 'WebAssembly renders from a named scene camera without a server')
  check(sceneCamera.before.objects[0].camera.fov === 50 && sceneCamera.after.objects[0].camera.fov === 50, 'scene camera lenses survive one Undo in WebAssembly')

  const analyticLight=await page.evaluate(async()=>{
    const post=commands=>fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands})}).then(r=>r.json())
    await post([{op:'clear'},{op:'add_light',name:'Static key',translation:[0,2,3],lamp:{color:'#ff6633',intensity:25}},{op:'add',primitive:{kind:'cube'}}])
    const before=await(await fetch('/api/scene')).json()
    const rendered=await fetch('/api/render?views=front&size=64')
    const bytes=Array.from(new Uint8Array(await rendered.arrayBuffer()).slice(0,4))
    await post([{op:'light_settings',id:'Static key',lamp:{kind:'sun',intensity:4}}])
    await fetch('/api/undo',{method:'POST'})
    const after=await(await fetch('/api/scene')).json()
    return {before,after,status:rendered.status,bytes}
  })
  check(analyticLight.status===200 && analyticLight.bytes.join(',')==='137,80,78,71','WebAssembly agent views trace editable analytic lights without a server')
  check(analyticLight.after.objects[0].light.color==='#ff6633' && analyticLight.after.objects[0].light.kind==='point','light settings and one Undo work through the WebAssembly command core')
  check(apiHits === 0, `no request reached a server API (${apiHits})`)
  check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join('; ')}` : ''}`)
} finally {
  await browser.close()
  server.close()
}
process.exit(failed ? 1 : 0)
