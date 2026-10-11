// Native/WASM project files must preserve the editable recipe through real Save/Open.
import fs from 'node:fs'
const TINY='iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEklEQVR4nGP4z8DAAMIM/4EAAB/uBfsL2WiLAAAAAElFTkSuQmCC'
export async function checkProjects(page,check) {
  const command=commands=>page.evaluate(async commands=>{
    const r=await fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands,source:'UI',actor:{id:'file-artist',name:'Saved Artist'}})});if(!r.ok)throw new Error(await r.text());await window.__tatara.refresh(false)
  },commands)
  const scene=()=>page.evaluate(()=>fetch('/api/scene').then(r=>r.json()))
  const project=()=>page.evaluate(()=>fetch('/api/project').then(r=>r.json()))
  const wait=async predicate=>{for(let i=0;i<600;i++){const s=await scene();if(predicate(s)&&await page.evaluate(rev=>document.querySelector('#status-rev').textContent===`Revision ${rev}`,s.revision))return s;await page.waitForTimeout(50)}throw new Error('project file did not settle')}
  await page.evaluate(async()=>{const r=await fetch('/api/reset',{method:'POST'});if(!r.ok)throw new Error(await r.text());await window.__tatara.refresh(false)})
  await command([{op:'add',name:'Project box',primitive:{kind:'cube'}}])
  await command([{op:'extrude',id:'Project box',face:4,distance:.5}])
  await command([
    {op:'add_image',name:'Saved image',data:TINY},
    {op:'add_camera',name:'Saved shot'},
    {op:'set_keyframe',id:'Saved shot',property:'camera_fov',frame:1,value:20,interpolation:'bezier'},
    {op:'set_keyframe',id:'Saved shot',property:'camera_fov',frame:11,value:60},
    {op:'set_key_handle',id:'Saved shot',property:'camera_fov',frame:1,side:'out',handle:{time:[.8],value:[40]}},
    {op:'set_key_handle',id:'Saved shot',property:'camera_fov',frame:11,side:'in',handle:{time:[.1],value:[0]}}
  ])
  const [download]=await Promise.all([page.waitForEvent('download'),page.click('[data-action=save]')])
  const bytes=fs.readFileSync(await download.path()),saved=JSON.parse(bytes)
  check(saved.format==='tatara-project'&&saved.version===1&&saved.steps.length===3,'Save downloads a versioned project with its editable command recipe')
  check(saved.steps[0].actor.name==='Saved Artist','project files retain command authors')
  await command([{op:'clear'},{op:'add',name:'Previous scene',primitive:{kind:'cube'}}])
  const previous=await scene(),previousRecipe=await project()
  await page.setInputFiles('#open-input',{name:'scene.tatara.json',mimeType:'application/json',buffer:bytes})
  const opened=await wait(s=>s.objects.some(o=>o.name==='Project box'))
  check(opened.objects.find(o=>o.name==='Project box').mesh.faces.length===10,'Open restores the saved scene by replaying its recipe')
  check((await project()).steps.length===3,'Open keeps the saved steps available for parametric editing')
  check(!!opened.images['Saved image'],'project replay retains image assets')
  const sampled=await page.evaluate(()=>fetch('/api/context?frame=8.625').then(r=>r.json()))
  check(Math.abs(sampled.objects.find(o=>o.name==='Saved shot').pose.camera.fov-55)<1e-9,'project files retain component-wise Bezier controls and sampled optics')
  await page.click('[data-action=undo]');await wait(s=>s.objects.some(o=>o.name==='Previous scene'))
  check(JSON.stringify((await project()).steps)===JSON.stringify(previousRecipe.steps),'one Undo restores both the previous scene and its recipe')
  await page.click('[data-action=redo]');await wait(s=>s.objects.some(o=>o.name==='Project box'))
  check((await project()).steps.length===3,'Redo restores the opened project and editable history')
  await page.click('.tabs [data-tab=history]');await page.click('[data-hs-open="1"]')
  const selector='[data-path=\'[0,"primitive","size"]\']'
  await page.fill(selector,'2');await page.click('[data-hs=apply]')
  const revised=await wait(s=>s.objects.find(o=>o.name==='Project box')?.mesh.vertices.some(v=>v[0]===1))
  const top=Math.max(...revised.objects.find(o=>o.name==='Project box').mesh.vertices.map(v=>v[1]))
  check(Math.abs(top-1.5)<1e-9,'editing a reopened first step replays the later extrusion through the History UI')
  const before=await scene(),beforeRecipe=await project(),invalid={...saved,version:99}
  await page.setInputFiles('#open-input',{name:'invalid.tatara.json',mimeType:'application/json',buffer:Buffer.from(JSON.stringify(invalid))})
  await page.waitForFunction(()=>document.querySelector('#toasts')?.textContent.includes('unsupported Tatara project version'))
  check(JSON.stringify(await scene())===JSON.stringify(before)&&JSON.stringify(await project())===JSON.stringify(beforeRecipe),'an unsupported project version leaves both the live scene and recipe unchanged')
  await page.setInputFiles('#open-input',{name:'legacy.tatara.json',mimeType:'application/json',buffer:Buffer.from(JSON.stringify(before))})
  await wait(s=>s.revision>before.revision)
  check((await project()).steps.length===0&&(await scene()).objects.some(o=>o.name==='Project box'),'legacy scene snapshots still open')
  const revision=(await scene()).revision
  await command([{op:'move',id:'Project box',offset:[1,0,0]}])
  const latest=await scene()
  const refused=await page.evaluate(async({saved,revision})=>{
    const r=await fetch('/api/project',{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({project:saved,expected_revision:revision})});return {status:r.status,data:await r.json()}
  },{saved,revision})
  check(refused.status===409&&JSON.stringify(await scene())===JSON.stringify(latest),'a stale project replacement refuses to overwrite a newer edit')
  const latestRecipe=await project()
  const exhausted=await page.evaluate(async saved=>{
    const body=JSON.stringify({project:saved}).replace(/"next_id":\d+/, '"next_id":18446744073709551615')
    const r=await fetch('/api/project',{method:'PUT',headers:{'Content-Type':'application/json'},body});return {status:r.status,data:await r.json()}
  },saved)
  check(exhausted.status===422&&exhausted.data.error.includes('exhausted')&&JSON.stringify(await scene())===JSON.stringify(latest)&&JSON.stringify(await project())===JSON.stringify(latestRecipe),'exhausted imported object IDs reject without wrapping or changing the project')
}
