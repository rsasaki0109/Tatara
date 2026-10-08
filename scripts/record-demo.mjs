#!/usr/bin/env node
// Record the README GIFs from the real editor.
//
// Each scenario in web/src/scenarios.js runs in headless Chromium against a
// fresh `tatara` server. Time is virtual: the page only advances when we call
// tick(), so every frame is rendered completely and recordings are identical
// from run to run. Frames are encoded with a two-pass ffmpeg palette.
//
//   node scripts/record-demo.mjs                 # all scenarios
//   node scripts/record-demo.mjs hero modeling   # some
//   options: --fps 15  --keep-frames  --mp4

import { spawn, spawnSync } from 'node:child_process'
import fs from 'node:fs'
import net from 'node:net'
import path from 'node:path'
import readline from 'node:readline'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const require = createRequire(path.join(root, 'web/package.json'))
const { chromium } = require('playwright-core')

const argv = process.argv.slice(2)
const flag = (name) => argv.includes(name)
const option = (name, fallback) => {
  const i = argv.indexOf(name)
  return i >= 0 ? argv[i + 1] : fallback
}
const fps = Number(option('--fps', 15))
const wanted = argv.filter((a, i) => !a.startsWith('--') && !(i > 0 && argv[i - 1] === '--fps'))
const VIEW = { width: 1280, height: 720 }
const outDir = path.join(root, 'docs/media')

function log(...a) {
  console.log('[record]', ...a)
}

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { stdio: 'inherit', cwd: root, ...opts })
  if (r.status !== 0) throw new Error(`${cmd} ${args.join(' ')} failed`)
}

function findBrowser() {
  if (process.env.TATARA_BROWSER_PATH) return process.env.TATARA_BROWSER_PATH
  const base = '/opt/pw-browsers'
  if (fs.existsSync(base)) {
    for (const d of fs.readdirSync(base).sort().reverse()) {
      const exe = path.join(base, d, 'chrome-linux/chrome')
      if (d.startsWith('chromium-') && fs.existsSync(exe)) return exe
    }
  }
  return undefined // playwright's own download, if installed
}

function freePort() {
  return new Promise((resolve) => {
    const s = net.createServer()
    s.listen(0, '127.0.0.1', () => {
      const { port } = s.address()
      s.close(() => resolve(port))
    })
  })
}

async function startServer(bin) {
  const port = await freePort()
  const child = spawn(bin, [], {
    cwd: root,
    env: { ...process.env, TATARA_PORT: String(port), TATARA_WEB_DIR: path.join(root, 'web/dist'), TATARA_AI_BASE_URL: '' },
    stdio: ['ignore', 'ignore', 'pipe'],
  })
  const url = `http://127.0.0.1:${port}`
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${url}/api/state`)).ok) return { child, url }
    } catch {}
    await new Promise((r) => setTimeout(r, 100))
  }
  child.kill()
  throw new Error('tatara server did not start')
}

/** A real `tatara --mcp` process, driven over stdio JSON-RPC. */
function startMcp(bin, url) {
  const child = spawn(bin, ['--mcp'], { env: { ...process.env, TATARA_URL: url }, stdio: ['pipe', 'pipe', 'inherit'] })
  const pending = new Map()
  let next = 1
  readline.createInterface({ input: child.stdout }).on('line', (line) => {
    const msg = JSON.parse(line)
    pending.get(msg.id)?.(msg)
    pending.delete(msg.id)
  })
  const rpc = (method, params) =>
    new Promise((resolve) => {
      const id = next++
      pending.set(id, resolve)
      child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
    })
  const ready = rpc('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'record-demo', version: '1' } })
  return {
    child,
    async call(tool, args) {
      await ready
      const msg = await rpc('tools/call', { name: tool, arguments: args })
      if (msg.error) return { text: msg.error.message, isError: true }
      const content = msg.result.content
      const image = content.find((c) => c.type === 'image')
      return {
        text: content.find((c) => c.type === 'text')?.text ?? '',
        image: image ? `data:${image.mimeType};base64,${image.data}` : undefined,
        isError: msg.result.isError,
      }
    },
  }
}

async function record(browser, bin, id) {
  const server = await startServer(bin)
  let mcp
  const page = await browser.newPage({ viewport: VIEW, deviceScaleFactor: 1 })
  page.on('pageerror', (e) => console.error(`[page] ${e.message}`))
  if (process.env.DEBUG_RECORD) page.on('console', (m) => console.log(`[console] ${m.text()}`))
  try {
    await page.goto(`${server.url}/?capture=1`)
    await page.evaluate(() => window.__tatara.ready)
    const meta = await page.evaluate((id) => window.__tatara.meta(id), id)
    if (!meta) throw new Error(`unknown scenario ${id}`)
    if (meta.external) {
      mcp = startMcp(bin, server.url)
      await page.exposeFunction('tataraMcp', (tool, args) => mcp.call(tool, args))
    }
    const frames = path.join(root, 'docs/frames', id)
    fs.rmSync(frames, { recursive: true, force: true })
    fs.mkdirSync(frames, { recursive: true })

    await page.evaluate((id) => window.__tatara.play(id), id)
    const started = Date.now()
    let n = 0
    for (;;) {
      // Poll instead of awaiting the page's promise: with an exposed binding
      // in play, Chromium can collect a long-awaited evaluate promise.
      await page.evaluate((ms) => window.__tatara.startTick(ms), 1000 / fps)
      const st = await (await page.waitForFunction(() => window.__tatara.tickResult, null, { polling: 5, timeout: 300000 })).jsonValue()
      if (st.error) throw new Error(`scenario ${id} failed: ${st.error}`)
      await page.screenshot({ path: path.join(frames, `${String(++n).padStart(4, '0')}.png`) })
      if (st.done) break
      if (n > fps * 60) throw new Error(`scenario ${id} ran longer than 60 s`)
    }
    log(`${id}: ${n} frames (${(n / fps).toFixed(1)} s) in ${((Date.now() - started) / 1000).toFixed(0)} s`)

    fs.mkdirSync(outDir, { recursive: true })
    const gif = path.join(outDir, `${id}.gif`)
    const filters =
      `scale=${meta.width}:-1:flags=lanczos,split[a][b];` +
      `[a]palettegen=max_colors=256:stats_mode=diff[p];` +
      `[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle`
    run('ffmpeg', ['-y', '-v', 'error', '-framerate', String(fps), '-i', path.join(frames, '%04d.png'), '-vf', filters, '-loop', '0', gif])
    log(`${path.relative(root, gif)}: ${(fs.statSync(gif).size / 1e6).toFixed(2)} MB`)
    if (flag('--mp4')) {
      const mp4 = path.join(outDir, `${id}.mp4`)
      run('ffmpeg', ['-y', '-v', 'error', '-framerate', String(fps), '-i', path.join(frames, '%04d.png'), '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-crf', '20', '-movflags', '+faststart', mp4])
      log(`${path.relative(root, mp4)}: ${(fs.statSync(mp4).size / 1e6).toFixed(2)} MB`)
    }
    if (!flag('--keep-frames')) fs.rmSync(frames, { recursive: true, force: true })
  } finally {
    await page.close()
    mcp?.child.kill()
    server.child.kill()
  }
}

async function main() {
  if (spawnSync('ffmpeg', ['-version']).status !== 0) throw new Error('ffmpeg is required')
  run('cargo', ['build', '--release', '--quiet'])
  if (!fs.existsSync(path.join(root, 'web/dist/index.html'))) run('npm', ['--prefix', 'web', 'run', 'build'])
  const bin = path.join(root, 'target/release', process.platform === 'win32' ? 'tatara.exe' : 'tatara')

  const browser = await chromium.launch({
    executablePath: findBrowser(),
    args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist', '--font-render-hinting=none'],
  })
  try {
    const probe = await startServer(bin)
    const page = await browser.newPage()
    await page.goto(probe.url)
    const all = await page.evaluate(() => window.__tatara.scenarios)
    await page.close()
    probe.child.kill()
    const ids = wanted.length ? wanted : all
    for (const id of ids) await record(browser, bin, id)
  } finally {
    await browser.close()
  }
}

main().catch((e) => {
  console.error(e)
  process.exit(1)
})
