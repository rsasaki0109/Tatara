// Joint bounds use the same Rig controls and sampled poses in native/WASM builds.
export async function checkJointLimits(page, check) {
  const command=async commands=>page.evaluate(async commands=>{
    const r=await fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands})});if(!r.ok)throw new Error(await r.text());await window.__tatara.refresh(false);return(await fetch('/api/scene')).json()
  },commands)
  const scene=()=>page.evaluate(()=>fetch('/api/scene').then(r=>r.json()))
  const wait=async predicate=>{for(let i=0;i<600;i++){const s=await scene();if(predicate(s)&&await page.evaluate(rev=>document.querySelector('#status-rev').textContent===`Revision ${rev}`,s.revision))return s;await page.waitForTimeout(50)}throw new Error('joint limit edit did not settle')}
  const initial=await command([
    {op:'clear'},{op:'set_animation',start:1,end:3},
    {op:'add',name:'Leg',primitive:{kind:'cylinder',height:2,rings:8},translation:[0,1,0]},
    {op:'rig',id:'Leg',chain:2},
    {op:'add',name:'Foot',primitive:{kind:'sphere'},translation:[0,1.4,0]},
    {op:'add',name:'Knee',primitive:{kind:'sphere'},translation:[1,.7,0]},
    {op:'track_target',id:'Leg',bone:'Bone 2',target:'Foot',pole:'Knee',chain:2}
  ])
  const id=initial.objects[0].id
  await page.click(`#outliner li[data-id="${id}"]`);await page.click('[data-bone="Bone 2"]')
  for(let k=0;k<3;k++){await page.fill(`#joint-min-${k}`,'0');await page.fill(`#joint-max-${k}`,k===2?'60':'0')}
  await page.evaluate(()=>window.__tatara.refresh(false))
  check(await page.inputValue('#joint-max-2')==='60','pending degree bounds survive a scene refresh')
  await page.click('[data-rig=limits]');const bounded=await wait(s=>s.objects[0].bones[1].limits?.max[2]>1)
  const z=bounded.objects[0].bones[1].rotation[2],max=bounded.objects[0].bones[1].limits.max[2]
  check(Math.abs(max-Math.PI/3)<1e-10&&z>=0&&z<=max,'Rig fields set local joint bounds in radians with one command')
  for(let i=0;i<600;i++){if(await page.evaluate(()=>Number(document.querySelector('#ik-error')?.textContent.match(/[\d.]+/)?.[0])>.2))break;await page.waitForTimeout(50)}
  const error=await page.textContent('#ik-error')
  const tip=await page.evaluate(id=>window.__tatara.rigTip(id),id)
  check(Number(error.match(/[\d.]+/)[0])>.2&&Math.hypot(...tip.map((v,k)=>v-[0,1.4,0][k]))>.2,'an infeasible IK target respects the bounds and reports its remaining distance')
  await page.click('[data-action=undo]');await wait(s=>!s.objects[0].bones[1].limits)
  check(true,'one Undo removes the complete joint-bound edit')
  await page.click('[data-action=redo]');await wait(s=>s.objects[0].bones[1].limits?.max[2]>1)
  await page.fill('#joint-min-2','90');const before=await scene();await page.click('[data-rig=limits]')
  await page.waitForFunction(()=>document.querySelector('#toasts')?.textContent.includes('minimum no greater'))
  check(JSON.stringify(await scene())===JSON.stringify(before),'reversed degree bounds are rejected without changing the scene')
  const authored=await command([
    {op:'clear_target',id:'Leg'},
    {op:'set_keyframe',id:'Leg',bone:'Bone 2',property:'bone',frame:1,value:[0,0,-1],interpolation:'linear'},
    {op:'set_keyframe',id:'Leg',bone:'Bone 2',property:'bone',frame:3,value:[0,0,2]}
  ])
  const sample=async frame=>{
    await page.evaluate(frame=>window.__tatara.setFrame(frame),frame)
    return page.evaluate(id=>window.__tatara.rigPose(id),id)
  }
  // Test the displayed Turn slider as well: it uses that sampled viewport pose.
  await sample(3)
  check(Number(await page.inputValue('#p-bone-2'))===60&&JSON.stringify(await scene())===JSON.stringify(authored),'animated bones display the clipped angle while authored keys remain unchanged')
  await page.click('[data-rig=clearLimits]');await wait(s=>!s.objects[0].bones[1].limits)
  check(Number(await page.inputValue('#p-bone-2'))===115,'clearing limits restores the authored animated angle')
  const precise=await command([{op:'set_bone_limits',id:'Leg',bone:'Bone 2',limits:{min:[0,0,0],max:[0,0,Math.PI/7+1e-10]}}])
  await page.fill('#joint-max-0','10');await page.click('[data-rig=limits]')
  const preserved=await wait(s=>s.objects[0].bones[1].limits?.max[0]>.1)
  check(Math.abs(preserved.objects[0].bones[1].limits.max[2]-precise.objects[0].bones[1].limits.max[2])<1e-12,'editing one limit preserves the precise radians of untouched axes')
}
