// Real browser-only snapshots, cross-tab ownership and explicit recovery.
import fs from 'node:fs'
export async function checkRecovery(browser,url,check) {
const errors=[]
 let apiHits=0
 const contexts=[]

async function ready(page){page.on('pageerror',e=>errors.push(e.message));page.on('request',r=>{if(new URL(r.url()).pathname.includes('/api/'))apiHits++});await page.goto(url,{waitUntil:"domcontentloaded",timeout:60000});await page.evaluate(()=>window.__tatara.ready)}
async function commands(page,commands){await page.evaluate(async commands=>{const r=await fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands,actor:{id:'saved-artist',name:'Saved Artist'}})});if(!r.ok)throw new Error(await r.text());await window.__tatara.refresh(false)},commands)}
const scene=page=>page.evaluate(()=>fetch('/api/scene').then(r=>r.json()))
const project=page=>page.evaluate(()=>fetch('/api/project').then(r=>r.json()))
async function flush(page){await page.evaluate(()=>window.__tatara.recoveryFlush());await page.waitForFunction(()=>document.querySelector('.recovery-status').dataset.state==='saved')}
async function records(page){return page.evaluate(async()=>{const db=await new Promise((resolve,reject)=>{const r=indexedDB.open('tatara-recovery',1);r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error)});try{return await new Promise((resolve,reject)=>{const r=db.transaction('sessions').objectStore('sessions').getAll();r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error)})}finally{db.close()}})}
try{
 const context=await browser.newContext({viewport:{width:1280,height:720},acceptDownloads:true});contexts.push(context)
 const a=await context.newPage();await ready(a)
 check((await records(a)).length===0,'an empty first startup does not create a recovery snapshot')
 await commands(a,[{op:'add',name:'Recovered box',primitive:{kind:'cube'}}])
 await commands(a,[{op:'extrude',id:'Recovered box',face:4,distance:.5}])
 await commands(a,[
  {op:'add_image',name:'Recovered image',data:'iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEklEQVR4nGP4z8DAAMIM/4EAAB/uBfsL2WiLAAAAAElFTkSuQmCC'},
  {op:'add_camera',name:'Recovered shot'},
  {op:'set_keyframe',id:'Recovered shot',property:'camera_fov',frame:1,value:20,interpolation:'bezier'},
  {op:'set_keyframe',id:'Recovered shot',property:'camera_fov',frame:11,value:60},
  {op:'set_key_handle',id:'Recovered shot',property:'camera_fov',frame:1,side:'out',handle:{time:[.8],value:[40]}},
  {op:'set_key_handle',id:'Recovered shot',property:'camera_fov',frame:11,side:'in',handle:{time:[.1],value:[0]}}
 ]);await flush(a)
 const archive=(await records(a))[0]
 check(archive.project.steps.length===3&&archive.project.steps[0].actor.name==='Saved Artist','automatic local snapshots include the editable recipe and authors')
 const b=await context.newPage();await ready(b)
 check((await scene(b)).objects.length===0&&await b.isVisible('.recovery-panel'),'another tab offers recovery without replacing its initial scene')
 await commands(b,[{op:'add',name:'Keep until restore',primitive:{kind:'sphere'}}])
 await b.click('[data-recover=restore]');await b.waitForFunction(()=>document.querySelector('.recovery-panel').hidden)
 check((await scene(b)).objects[0].mesh.faces.length===10&&(await project(b)).steps.length===3,'explicit recovery replays geometry and retains editable steps')
 await b.click('[data-action=undo]');check((await scene(b)).objects[0].name==='Keep until restore','one Undo restores the scene replaced by recovery')
 await b.click('[data-action=redo]');check((await scene(b)).objects[0].name==='Recovered box','Redo restores the recovered scene and its recipe')
 await b.evaluate(async()=>{const r=await fetch('/api/history/revise',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({step:1,commands:[{op:'add',name:'Recovered box',primitive:{kind:'cube',size:2}}]})});if(!r.ok)throw new Error(await r.text());await window.__tatara.refresh(false)})
 check((await scene(b)).objects[0].mesh.vertices.some(v=>v[1]===1.5),'an earlier recovered step remains revisable and replays later extrusion')
 await flush(b);await commands(a,[{op:'move',id:'Recovered box',offset:[1,0,0]}]);await flush(a)
 const independent=await records(a)
 check(independent.length===2&&new Set(independent.map(r=>r.id)).size===2,'concurrent tabs own separate recovery records')
 const bBeforeMenu=await scene(b)
 await b.click('.recovery-status')
 await b.waitForFunction(()=>document.querySelector('.recovery-panel select').textContent.includes('4 editable steps'))
 check(JSON.stringify(await scene(b))===JSON.stringify(bBeforeMenu),'the recovery menu reads another tab’s latest snapshot without changing the current scene')
 await b.close()
 await a.reload({waitUntil:"domcontentloaded",timeout:60000});await a.evaluate(()=>window.__tatara.ready)
 check((await scene(a)).objects.length===0&&await a.isVisible('.recovery-panel'),'a reload offers snapshots without silently applying them')
 await a.selectOption('.recovery-panel select',archive.id)
 await a.click('[data-recover=restore]');await a.waitForFunction(()=>document.querySelector('.recovery-panel').hidden)
 check((await scene(a)).objects[0].transform.translation[0]===1&&(await project(a)).steps.length===4,'reload recovery restores the last settled edit and all four steps')
 check(!!(await scene(a)).images['Recovered image'],'local recovery retains uploaded image assets')
 const sample=await a.evaluate(()=>fetch('/api/context?frame=8.625').then(r=>r.json()))
 check(Math.abs(sample.objects.find(o=>o.name==='Recovered shot').pose.camera.fov-55)<1e-9,'local recovery retains Bezier controls and fractional camera sampling')
 await commands(a,[{op:'clear'}]);await flush(a)
 const latest=(await records(a)).sort((x,y)=>y.updatedAt-x.updatedAt)[0]
 check(latest.project.steps.at(-1).commands[0].op==='clear','intentional Clear is saved rather than resurrecting an earlier scene')
 const active=latest.id
 await a.evaluate(async archive=>{const db=await new Promise(resolve=>{const r=indexedDB.open('tatara-recovery',1);r.onsuccess=()=>resolve(r.result)});try{await new Promise((resolve,reject)=>{const tx=db.transaction('sessions','readwrite');for(let i=0;i<6;i++)tx.objectStore('sessions').put({id:'retention-'+i,updatedAt:Date.now()+100000+i,project:archive.project});tx.oncomplete=resolve;tx.onerror=()=>reject(tx.error)})}finally{db.close()}},archive)
 await commands(a,[{op:'add',name:'After retention',primitive:{kind:'cube'}}]);await flush(a)
 const retained=await records(a)
 check(retained.length===5&&retained.some(r=>r.id===active),'retention is bounded and keeps the active tab even when old timestamps are ahead')
 await a.evaluate(async id=>{const db=await new Promise(resolve=>{const r=indexedDB.open('tatara-recovery',1);r.onsuccess=()=>resolve(r.result)});try{await new Promise((resolve,reject)=>{const tx=db.transaction('sessions','readwrite'),store=tx.objectStore('sessions'),r=store.get(id);r.onsuccess=()=>{r.result.project.version=99;store.put(r.result)};tx.oncomplete=resolve;tx.onerror=()=>reject(tx.error)})}finally{db.close()}},active)
 const bad=await context.newPage();await ready(bad);await bad.selectOption('.recovery-panel select',active)
 await bad.click('[data-recover=restore]');await bad.waitForFunction(()=>document.querySelector('.recovery-message').textContent.includes('Could not restore'))
 check((await scene(bad)).objects.length===0&&(await project(bad)).steps.length===0,'an invalid cached project fails atomically without changing the current recipe')
 await bad.selectOption('.recovery-panel select','retention-5')
 await bad.evaluate(()=>{const previous=window.fetch;window.fetch=async(input,init)=>{if(String(input).endsWith('/api/project')&&init?.method==='PUT'){window.fetch=previous;await previous('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands:[{op:'add',name:'Newer edit',primitive:{kind:'cube'}}]})})}return previous(input,init)}})
 await bad.click('[data-recover=restore]');await bad.waitForFunction(()=>document.querySelector('.recovery-message').textContent.includes('revision'))
 check((await scene(bad)).objects[0].name==='Newer edit'&&(await project(bad)).steps.length===1,'recovery refuses a stale replacement with the shared revision guard')
 await bad.close()
 await a.evaluate(()=>{IDBObjectStore.prototype.put=function(){throw new DOMException('Quota exhausted','QuotaExceededError')}})
 await commands(a,[{op:'move',id:'After retention',offset:[1,0,0]}]);await a.evaluate(()=>window.__tatara.recoveryFlush())
 await a.waitForFunction(()=>document.querySelector('.recovery-status').dataset.state==='error')
 check((await scene(a)).objects[0].transform.translation[0]===1,'quota failures leave successful edits available and report the recovery failure')
 await a.setViewportSize({width:360,height:740})
 check(await a.evaluate(()=>document.documentElement.scrollWidth<=innerWidth&&document.body.scrollWidth<=innerWidth),'browser recovery status fits a phone viewport without horizontal scrolling')
 await a.setViewportSize({width:1280,height:720})
 await a.close()
 const denied=await browser.newContext({viewport:{width:1280,height:720}});contexts.push(denied)
 await denied.addInitScript(()=>Object.defineProperty(window,'indexedDB',{get(){throw new Error('Storage disabled')}}))
 const c=await denied.newPage();await ready(c)
 check(await c.textContent('.recovery-status')==='Local recovery unavailable','storage failure is visible and does not prevent startup')
 await commands(c,[{op:'add',name:'Still editable',primitive:{kind:'cube'}}])
 const [download]=await Promise.all([c.waitForEvent('download'),c.click('[data-action=save]')])
 check(JSON.parse(fs.readFileSync(await download.path())).steps.length===1,'manual project Save remains available when local storage fails')
 await c.close()
 const legacyCrypto=await browser.newContext({viewport:{width:1280,height:720}});contexts.push(legacyCrypto)
 await legacyCrypto.addInitScript(()=>Object.defineProperty(crypto,'randomUUID',{value:undefined}))
 const d=await legacyCrypto.newPage();await ready(d);await commands(d,[{op:'add',name:'Compatible ID',primitive:{kind:'cube'}}]);await flush(d)
 check((await records(d))[0].id.length===32,'browsers without randomUUID still use separate cryptographically random recovery keys')
 await d.reload({waitUntil:"domcontentloaded",timeout:60000});await d.evaluate(()=>window.__tatara.ready)
 const beforeDiscard=await scene(d)
 await d.click('[data-recover=discard]')
 await d.waitForFunction(()=>document.querySelector('[data-recover=discard]').disabled)
 check((await records(d)).length===0&&JSON.stringify(await scene(d))===JSON.stringify(beforeDiscard)&&await d.textContent('.recovery-status')==='Local recovery ready','Discard removes the chosen snapshot and updates availability without altering the current scene')
 check(apiHits===0,'recovery uses the real WASM backend without a server API')
 check(errors.length===0,'recovery and storage failures produce no page errors')
} finally { for (const context of contexts) await context.close() }
}
