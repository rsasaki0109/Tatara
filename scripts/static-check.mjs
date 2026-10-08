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
  await page.click('.tabs [data-tab=properties]')

  await page.click('#demo-btn')
  // The tour ends with a plinth, the sculpted head and the agent's three objects.
  await page.waitForFunction(() => document.querySelectorAll('#outliner li').length === 5, null, { timeout: 90000 })
  await page.waitForFunction(() => !document.querySelector('#demo-btn').disabled, null, { timeout: 90000 })
  check(true, 'the tour plays')
  check(apiHits === 0, `no request reached a server API (${apiHits})`)
  check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join('; ')}` : ''}`)
} finally {
  await browser.close()
  server.close()
}
process.exit(failed ? 1 : 0)
