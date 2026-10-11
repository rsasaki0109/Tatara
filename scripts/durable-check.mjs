// Real process restart: committed recipes survive without opening a scene file.
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import net from 'node:net'

export async function checkDurable(browser, bin, root, check) {
  const folder = fs.mkdtempSync(path.join(os.tmpdir(), 'tatara-durable-browser-'))
  const checkpoint = path.join(folder, 'workspace.db')
  const port = await new Promise(resolve => {
    const listener = net.createServer().listen(0, '127.0.0.1', () => {
      const port = listener.address().port
      listener.close(() => resolve(port))
    })
  })
  const url = `http://127.0.0.1:${port}`
  let child
  const start = async () => {
    child = spawn(bin, [], {cwd:root, env:{...process.env,TATARA_PORT:String(port),TATARA_PROJECT:checkpoint,TATARA_WEB_DIR:path.join(root,'web/dist'),TATARA_AI_BASE_URL:''},stdio:['ignore','ignore','inherit']})
    for (let i=0;i<200;i++) {
      if (child.exitCode !== null) throw new Error('durable server exited during startup')
      try { if ((await fetch(`${url}/api/state`)).ok) return } catch {}
      await new Promise(resolve=>setTimeout(resolve,50))
    }
    throw new Error('durable server did not start')
  }
  const stop = async () => {
    if (!child || child.exitCode !== null) return
    const exited=once(child,'exit');child.kill();await exited
  }
  const page = await browser.newPage({viewport:{width:1280,height:720}})
  const errors=[]
  page.on('pageerror',error=>errors.push(error.message))
  const json = async route => (await fetch(`${url}/api/${route}`)).json()
  const phase = async n => {
    await page.evaluate(n=>window.__tatara.play('nativeRestart',n),n)
    for(let i=0;i<1200;i++) {
      await page.evaluate(()=>window.__tatara.startTick(100))
      const result=await (await page.waitForFunction(()=>window.__tatara.tickResult,null,{polling:5})).jsonValue()
      if(result.error) throw new Error(result.error)
      if(result.done) return
    }
    throw new Error('native restart scenario did not finish')
  }
  try {
    await start()
    await page.goto(`${url}/?capture=1`)
    await page.evaluate(()=>window.__tatara.ready)
    check(await page.locator('#status-storage').isVisible(),'durable native workspace displays its saved status')
    await phase(0)
    const saved=await json('project'),scene=await json('scene'),health=await json('storage')
    check(health.available && health.saved_revision===scene.revision,'accepted native edits are durable before acknowledgment')
    await stop();await start()
    check(JSON.stringify(await json('project'))===JSON.stringify(saved),'real server restart restores the complete editable recipe')
    const restored=await json('scene')
    check(restored.revision===scene.revision+1 && restored.objects.length===scene.objects.length,'restart advances revision and restores scene objects')
    const stale=await fetch(`${url}/api/commands`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({expected_revision:scene.revision,commands:[{op:'clear'}]})})
    check(stale.status===409,'pre-restart commands cannot overwrite the recovered workspace')
    await page.reload();await page.evaluate(()=>window.__tatara.ready)
    check(await page.locator('#outliner li').count()===scene.objects.length,'browser reload displays the recovered native scene')
    await phase(1)
    check((await json('project')).steps[0].commands[0].primitive.size===1.5,'recovered history remains revisable and later steps replay')
    check(errors.length===0,'native restart scenario completes without browser errors')
  } finally {
    await page.close();await stop();fs.rmSync(folder,{recursive:true,force:true})
  }
}
