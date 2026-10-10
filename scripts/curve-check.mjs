// Exercise authored-key editing through actual controls and pointer events.
export async function checkCurves(page, check) {
  await page.evaluate(async()=>{
    const r=await fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands:[
      {op:'clear'},{op:'set_animation',start:1,end:48},
      {op:'add',name:'Curve ball',primitive:{kind:'sphere'}},
      {op:'set_keyframe',id:'Curve ball',property:'translation',frame:1,value:[0,1,0],interpolation:'linear'},
      {op:'set_keyframe',id:'Curve ball',property:'translation',frame:25,value:[2,1,0]},
      {op:'set_keyframe',id:'Curve ball',property:'rotation',frame:1,value:[0,0,0]},
      {op:'set_keyframe',id:'Curve ball',property:'rotation',frame:25,value:[0,Math.PI/2,0]}
    ]})});if(!r.ok)throw new Error(await r.text());await window.__tatara.refresh(false)
  })
  const scene=()=>page.evaluate(()=>fetch('/api/scene').then(r=>r.json()))
  const wait=async predicate=>{
    for(let i=0;i<600;i++){const s=await scene();if(predicate(s)&&await page.evaluate(rev=>document.querySelector('#status-rev').textContent===`Revision ${rev}`,s.revision))return s;await page.waitForTimeout(50)}
    throw new Error('curve command did not reach the expected scene')
  }
  const keys=s=>s.objects[0].tracks[0].keys
  await page.click('#outliner li')
  await page.click('[data-curve-key="25"]')
  await page.waitForFunction(()=>document.querySelector('#frame-input').value==='25')
  check(await page.evaluate(()=>document.querySelector('#frame-input').value==='25'),'clicking a curve key seeks its frame')
  await page.fill('#curve-frame','37');await page.fill('#curve-value','4');await page.selectOption('#curve-interpolation','step')
  await page.click('[data-curve-apply]')
  const edited=await wait(s=>keys(s)[1].frame===37)
  check(keys(edited)[1].value.join(',')==='4,1,0'&&keys(edited)[1].interpolation==='step'&&edited.objects[0].tracks[1].keys[1].frame===25,'curve fields retime one channel and preserve other components and tracks')
  await page.click('[data-action=undo]');await wait(s=>keys(s)[1].frame===25)
  check(true,'one Undo restores the complete key edit')
  await page.click('[data-curve-key="25"]');const before=await scene()
  await page.fill('#curve-frame','1');await page.click('[data-curve-apply]')
  await page.waitForFunction(()=>document.querySelector('#toasts')?.textContent.includes('a key already exists'))
  check(JSON.stringify(await scene())===JSON.stringify(before),'a destination-key collision rejects the edit without changing the scene')
  await page.click('[data-curve-key="25"]')
  const dot=await page.locator('[data-curve-key="25"]').boundingBox(),svg=await page.locator('[data-curve-plot]').boundingBox()
  await page.mouse.move(dot.x+dot.width/2,dot.y+dot.height/2);await page.mouse.down()
  await page.mouse.move(dot.x+dot.width/2+svg.width*300/360*6/47,dot.y+dot.height/2,{steps:8});await page.mouse.up()
  const dragged=await wait(s=>Math.abs(keys(s)[1].frame-31)<.15)
  check(keys(dragged)[1].value.join(',')==='2,1,0'&&dragged.revision===before.revision+1,'dragging a key submits one edit and preserves its value on a horizontal drag')
  await page.selectOption('#curve-channel','1');await page.selectOption('#curve-component','1');await page.click('[data-curve-key="25"]')
  check(await page.inputValue('#curve-value')==='90','rotation curves display degrees')
  await page.fill('#curve-value','45');await page.click('[data-curve-apply]')
  const rotated=await wait(s=>Math.abs(s.objects[0].tracks[1].keys[1].value[1]-Math.PI/4)<1e-10)
  check(rotated.objects[0].tracks[1].keys[1].value[0]===0,'degree edits reach the Rust rotation track in radians')
}
