#!/usr/bin/env node
// Build the browser-only Tatara: the Rust core compiled to WebAssembly plus
// the editor UI, as a static site in web/dist-static (no server needed).
//
//   node scripts/build-static.mjs
//   npx --prefix web vite preview --outDir dist-static   # or any static server

import { spawnSync } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const run = (cmd, args, env = {}) => {
  const r = spawnSync(cmd, args, { cwd: root, stdio: 'inherit', env: { ...process.env, ...env } })
  if (r.status !== 0) process.exit(r.status ?? 1)
}

run('cargo', ['build', '-p', 'tatara-wasm', '--target', 'wasm32-unknown-unknown', '--profile', 'wasm'])
const wasm = path.join(root, 'target/wasm32-unknown-unknown/wasm/tatara_wasm.wasm')
fs.mkdirSync(path.join(root, 'web/public'), { recursive: true })
fs.copyFileSync(wasm, path.join(root, 'web/public/tatara.wasm'))
run('npm', ['--prefix', 'web', 'run', 'build'], { VITE_TATARA_STATIC: '1', TATARA_BASE: './', TATARA_OUT_DIR: 'dist-static' })
fs.rmSync(path.join(root, 'web/public/tatara.wasm'))
const size = fs.statSync(path.join(root, 'web/dist-static/tatara.wasm')).size
console.log(`[static] web/dist-static ready (tatara.wasm ${(size / 1e6).toFixed(2)} MB)`)
