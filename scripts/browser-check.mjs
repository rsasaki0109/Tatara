#!/usr/bin/env node
// Browser smoke test against an isolated editor: real clicks and keys for
// editing, history and files, then every demo scenario in capture mode.
// Requires `npm --prefix web run build` and `cargo build --release`.

import { spawn } from 'node:child_process'
import fs from 'node:fs'
import net from 'node:net'
import path from 'node:path'
import { createRequire } from 'node:module'
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
  const faces = async () => Number((await page.textContent('#status-mesh')).match(/([\d,]+) faces/)[1].replace(/,/g, ''))

  await page.click('[data-add=cube]')
  await page.waitForFunction(() => document.querySelectorAll('#outliner li').length === 1)
  check((await count()) === 1, 'toolbar adds a cube')

  await page.keyboard.press('Escape') // detach the gizmo from the cube's centre
  const box = await page.locator('#viewport canvas').boundingBox()
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

  await page.click('#outliner li')
  await page.click('[data-add-mod=array]')
  await page.waitForFunction(() => /30 faces/.test(document.querySelector('#status-mesh').textContent))
  await page.click('[data-add-mod=subdivision]')
  await page.waitForFunction(() => /120 faces/.test(document.querySelector('#status-mesh').textContent))
  check((await faces()) === 120, 'modifier stack evaluates array → subdivision')
  await page.click('[data-mod-apply]')
  await page.waitForFunction(() => !document.querySelector('[data-mod-apply]'))
  check((await faces()) === 120, 'apply bakes the stack into the base mesh')

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
