// The same modifier controls must drive native and browser-only cores.
export async function checkSolidify(page,check) {
  const state=()=>page.evaluate(()=>fetch('/api/state').then(r=>r.json()))
  const wait=async predicate=>{for(let i=0;i<600;i++){const s=await state();if(predicate(s.scene||s))return s.scene||s;await page.waitForTimeout(50)}throw new Error('solidify did not settle')}
  await page.evaluate(async()=>{const r=await fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands:[{op:'clear'},{op:'add_mesh',name:'Shell panel',vertices:[[-1,.6,-1],[1,.6,-1],[1,.6,1],[-1,.6,1]],faces:[[0,3,2,1]]}]})});if(!r.ok)throw new Error(await r.text());await window.__tatara.refresh(false)})
  await page.click('.tabs [data-tab=properties]')
  await page.click('#outliner li');await page.click('[data-add-mod=solidify]')
  const added=await wait(s=>s.objects[0]?.display?.faces.length===6)
  check(added.objects[0].mesh.faces.length===1,'Solidify closes a sheet while retaining its editable base')
  const input='[data-mod="0"][data-field=thickness]'
  await page.fill(input,'0.2');await page.press(input,'Tab')
  const thick=await wait(s=>s.objects[0].modifiers[0]?.thickness===.2)
  check(Math.abs(Math.min(...thick.objects[0].display.vertices.map(v=>v[1]))-.4)<1e-8,'Thickness controls the inward shell depth')
  await page.click('[data-action=undo]');await wait(s=>s.objects[0].modifiers[0]?.thickness===.05)
  check(true,'one Undo restores the previous thickness')
  await page.click('[data-action=redo]');await wait(s=>s.objects[0].modifiers[0]?.thickness===.2)
  await page.click('[data-mod-apply]');await wait(s=>s.objects[0].mesh.faces.length===6&&!s.objects[0].modifiers.length)
  check(true,'Apply stack bakes the closed shell into the base mesh')
  await page.click('[data-action=undo]');await wait(s=>s.objects[0].mesh.faces.length===1&&s.objects[0].modifiers.length===1)
  check(true,'Undo restores the editable Solidify stack after baking')
  const rejected=await page.evaluate(async()=>{const before=await fetch('/api/project').then(r=>r.json());const r=await fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands:[{op:'move',id:'Shell panel',offset:[1,0,0]},{op:'set_modifier',id:'Shell panel',index:0,modifier:{type:'solidify',thickness:0}}]})});return r.status===422&&JSON.stringify(before)===JSON.stringify(await fetch('/api/project').then(r=>r.json()))})
  check(rejected,'invalid thickness rejects the whole batch without changing its recipe')
}
