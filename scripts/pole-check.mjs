// Actual Rig controls and core-resolved viewport poses, shared with WASM.
export async function checkPoles(page, check) {
  const command=async commands=>page.evaluate(async commands=>{
    const r=await fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands})});if(!r.ok)throw new Error(await r.text());await window.__tatara.refresh(false);return(await fetch('/api/scene')).json()
  },commands)
  const scene=()=>page.evaluate(()=>fetch('/api/scene').then(r=>r.json()))
  const wait=async predicate=>{for(let i=0;i<600;i++){const s=await scene();if(predicate(s)&&await page.evaluate(rev=>document.querySelector('#status-rev').textContent===`Revision ${rev}`,s.revision))return s;await page.waitForTimeout(50)}throw new Error('pole command did not settle')}
  const initial=await command([
    {op:'clear'},{op:'set_animation',start:1,end:3},
    {op:'add',name:'Leg',primitive:{kind:'cylinder',height:2,rings:8},translation:[0,1,0]},
    {op:'rig',id:'Leg',chain:2},
    {op:'add',name:'Foot',primitive:{kind:'sphere'},translation:[0,1.4,0]},
    {op:'add',name:'Knee',primitive:{kind:'sphere'},translation:[1,.7,0]}
  ])
  const id=initial.objects[0].id,pole=initial.objects[2].id
  await page.click(`#outliner li[data-id="${id}"]`);await page.click('[data-bone="Bone 2"]')
  await page.selectOption('#ik-target',String(initial.objects[1].id));await page.selectOption('#ik-pole',String(pole));await page.evaluate(()=>window.__tatara.refresh(false));
  check(await page.inputValue('#ik-target')===String(initial.objects[1].id)&&await page.inputValue('#ik-pole')===String(pole),'pending target choices survive a scene refresh before Follow');await page.click('[data-rig=follow]')
  const bound=await wait(s=>s.ik_targets?.[0]?.pole===pole)
  check(bound.ik_targets[0].chain===2,'Rig controls bind a pole to the selected two-bone chain')
  // Use the displayed root rotation to distinguish the two bend planes.
  const bend=()=>page.evaluate(id=>window.__tatara.rigPose(id).bones[0].rotation[2],id)
  const right=await bend();await command([{op:'move',id:'Knee',offset:[-2,0,0]}]);const left=await bend()
  const tip=await page.evaluate(id=>window.__tatara.rigTip(id),id)
  check(right*left<0&&Math.hypot(...tip.map((v,k)=>v-[0,1.4,0][k]))<1e-7,'moving the pole reverses the displayed bend while the tip stays on its target')
  await page.click('[data-action=undo]');await wait(s=>s.objects[2].transform.translation[0]===1)
  check(Math.abs(await bend()-right)<1e-8,'one Undo restores the pole and solved pose')
  const authored=await command([
    {op:'set_keyframe',id:'Knee',property:'translation',frame:1,value:[1,.7,0],interpolation:'linear'},
    {op:'set_keyframe',id:'Knee',property:'translation',frame:3,value:[-1,.7,0]}
  ])
  const seek=async frame=>{
    await page.evaluate(frame=>window.__tatara.setFrame(frame),frame)
    for(let i=0;i<600;i++){if(await page.evaluate(frame=>window.__tatara.resolvedFrame()===frame,frame))return;await page.waitForTimeout(50)}throw new Error('pole frame did not resolve')
  }
  await seek(3);const last=await bend();await seek(1);const first=await bend();await seek(3)
  check(first*last<0&&Math.abs(await bend()-last)<1e-8&&JSON.stringify(await scene())===JSON.stringify(authored),'animated poles seek deterministically without editing authored bones or revision')
  await command([{op:'delete',id:'Knee'}]);check((await scene()).ik_targets.length===1&&!(await scene()).ik_targets[0].pole,'deleting the pole retains the IK target')
  await page.click('[data-action=undo]');await wait(s=>s.ik_targets?.[0]?.pole===pole)
  check(true,'Undo restores a deleted pole binding')
}
