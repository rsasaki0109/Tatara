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
  const faceReplay = await page.evaluate(async () => {
    const post=(path,body)=>fetch(`/api/${path}`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)}).then(r=>r.json())
    const primitive=segments=>({op:'add',name:'Cylinder',primitive:{kind:'cylinder',segments}})
    await post('commands',{commands:[{op:'clear'},primitive(9)]})
    const history=await(await fetch('/api/history?limit=1')).json()
    await post('commands',{commands:[{op:'extrude',id:'Cylinder',face:{normal:[0,1,0],centre:[.2943407405,.5,.1071312683],max_distance:.1},distance:.4}]})
    const before=await(await fetch('/api/scene')).json()
    await post('history/revise',{step:history.steps[0].step,commands:[{op:'clear'},primitive(12)]})
    const after=await(await fetch('/api/scene')).json()
    await fetch('/api/undo',{method:'POST'})
    const undone=await(await fetch('/api/scene')).json()
    return {before,after,undone}
  })
  check(faceReplay.before.objects[0].mesh.faces.length !== faceReplay.after.objects[0].mesh.faces.length && Math.abs(Math.max(...faceReplay.after.objects[0].mesh.vertices.map(p=>p[1]))-.9)<1e-6,'WebAssembly reidentifies geometric faces during history replay')
  check(JSON.stringify(faceReplay.before.objects) === JSON.stringify(faceReplay.undone.objects),'WebAssembly Undo restores topology-sensitive face edits')
  const orientation = await page.evaluate(async () => {
    const post=commands=>fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands})}).then(r=>r.json())
    await post([{op:'clear'},{op:'add_camera',name:'Shot'},{op:'add_light',name:'Key',rotation:[0,.3,0]},{op:'constrain',id:'Key',orientation:'Shot'}])
    await post([{op:'transform',id:'Shot',rotation:[0,.4,0]}])
    const turned=await(await fetch('/api/scene')).json()
    await fetch('/api/undo',{method:'POST'})
    const undone=await(await fetch('/api/scene')).json()
    return {turned,undone}
  })
  check(Math.abs(orientation.turned.objects[1].transform.rotation[1]-.7)<1e-6,'WebAssembly keeps camera/light relative orientation')
  check(Math.abs(orientation.undone.objects[1].transform.rotation[1]-.3)<1e-6,'WebAssembly Undo restores both orientation endpoints')
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
  const optics=await page.evaluate(async()=>{
    const post=commands=>fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands})}).then(r=>r.json())
    await post([{op:'clear'},{op:'add_camera',name:'Optical shot',translation:[0,1,4]},{op:'add_light',name:'Optical key'},
      {op:'set_keyframe',id:'Optical shot',property:'camera_fov',frame:1,value:20,interpolation:'linear'},
      {op:'set_keyframe',id:'Optical shot',property:'camera_fov',frame:3,value:60},
      {op:'set_keyframe',id:'Optical key',property:'light_intensity',frame:1,value:0,interpolation:'linear'},
      {op:'set_keyframe',id:'Optical key',property:'light_intensity',frame:3,value:40}])
    const context=await(await fetch('/api/context?frame=2')).json()
    const rendered=await fetch('/api/render/image?camera=Optical%20shot&frame=2&w=8&h=8&samples=1')
    const signature=Array.from(new Uint8Array(await rendered.arrayBuffer()).slice(0,4))
    const before=await(await fetch('/api/scene')).json()
    const bad=await fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands:[{op:'set_keyframe',id:'Optical key',property:'camera_fov',frame:2,value:40}]})})
    const after=await(await fetch('/api/scene')).json()
    return {context,status:rendered.status,signature,bad:bad.status,unchanged:JSON.stringify(before)===JSON.stringify(after)}
  })
  check(optics.context.objects[0].camera.fov===40 && optics.context.objects[1].light.intensity===20,'WebAssembly samples lens and light properties at the requested frame')
  check(optics.status===200 && optics.signature.join(',')==='137,80,78,71','WebAssembly renders an animated named camera')
  check(optics.bad===422 && optics.unchanged,'WebAssembly rejects optical keys on the wrong object atomically')
  const orthographic=await page.evaluate(async()=>{
    const post=commands=>fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands})})
    await post([{op:'clear'},{op:'add_camera',name:'Drawing',translation:[0,1,4],lens:{ortho_height:4}},
      {op:'set_keyframe',id:'Drawing',property:'camera_height',frame:1,value:2,interpolation:'linear'},
      {op:'set_keyframe',id:'Drawing',property:'camera_height',frame:3,value:6},{op:'add',primitive:{kind:'cube'}}])
    const context=await(await fetch('/api/context?frame=2')).json()
    const image=await fetch('/api/render/image?camera=Drawing&frame=2&w=8&h=8&samples=1')
    const signature=Array.from(new Uint8Array(await image.arrayBuffer()).slice(0,4))
    const before=await(await fetch('/api/scene')).json(),bad=await post([{op:'camera_settings',id:'Drawing',lens:{ortho_height:0}}]),after=await(await fetch('/api/scene')).json()
    return {context,status:image.status,signature,bad:bad.status,unchanged:JSON.stringify(before)===JSON.stringify(after)}
  })
  check(orthographic.context.objects[0].camera.ortho_height===4,'WebAssembly samples orthographic world-space view height')
  check(orthographic.status===200 && orthographic.signature.join(',')==='137,80,78,71','WebAssembly renders the named orthographic projection')
  check(orthographic.bad===422 && orthographic.unchanged,'WebAssembly rejects invalid orthographic height atomically')
  const spotlight=await page.evaluate(async()=>{
    const post=commands=>fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands})})
    await post([{op:'clear'},{op:'add_light',name:'Spot',translation:[0,2,3],rotation:[-.4,0,0],lamp:{kind:'spot',inner_cone:.2,outer_cone:.8,range:8,intensity:80}},{op:'add',primitive:{kind:'cube'}}])
    const before=await(await fetch('/api/scene')).json()
    const image=await fetch('/api/render/image?view=front&w=8&h=8&samples=1'),signature=Array.from(new Uint8Array(await image.arrayBuffer()).slice(0,4))
    const bad=await post([{op:'light_settings',id:'Spot',lamp:{kind:'spot',inner_cone:.8,outer_cone:.8}}])
    return{before,status:image.status,signature,bad:bad.status,unchanged:JSON.stringify(before)===JSON.stringify(await(await fetch('/api/scene')).json())}
  })
  check(spotlight.before.objects[0].light.kind==='spot' && spotlight.before.objects[0].light.range===8,'WebAssembly retains spot cones and finite range')
  check(spotlight.status===200 && spotlight.signature.join(',')==='137,80,78,71','WebAssembly renders spot lights without a server')
  check(spotlight.bad===422 && spotlight.unchanged,'WebAssembly rejects equal spot cones atomically')

  const frameRest=await page.evaluate(async()=>{
    const r=await fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands:[
      {op:'clear'}, {op:'add',name:'Anchor',primitive:{kind:'cube'}},
      {op:'add',name:'Follower',primitive:{kind:'cube'},translation:[2,0,0]},
      {op:'constrain',id:'Follower',align:'y',from:'Anchor'},
      {op:'set_keyframe',id:'Anchor',property:'translation',frame:1,value:[0,0,0],interpolation:'linear'},
      {op:'set_keyframe',id:'Anchor',property:'translation',frame:3,value:[0,2,0]}
    ]})});if(!r.ok)throw new Error(await r.text())
    await window.__tatara.refresh(false)
    const rest=await(await fetch('/api/scene')).json();window.__tatara.setFrame(2);return rest
  })
  await page.waitForFunction(()=>window.__tatara.resolvedFrame()===2)
  const frames=await page.evaluate(async id=>({position:window.__tatara.position(id),frame:await(await fetch('/api/frame?frame=2')).json(),rest:await(await fetch('/api/scene')).json()}),frameRest.objects[1].id)
  check(frames.position[1]===1 && frames.frame.objects[1].transform.translation[1]===1,'WebAssembly solves constraints per frame and updates the unkeyed viewport follower')
  check(JSON.stringify(frames.rest)===JSON.stringify(frameRest),'WebAssembly frame reads preserve the authored scene and revision')

  check(apiHits === 0, `no request reached a server API (${apiHits})`)
  check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join('; ')}` : ''}`)
} finally {
  await browser.close()
  server.close()
}
process.exit(failed ? 1 : 0)
