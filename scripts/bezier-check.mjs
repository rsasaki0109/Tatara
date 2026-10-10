// Real pointer edits and shared sampling in both native and WASM builds.
export async function checkBezier(page, check) {
  const command=commands=>page.evaluate(async commands=>{
    const r=await fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands})});if(!r.ok)throw new Error(await r.text());await window.__tatara.refresh(false)
  },commands)
  const scene=()=>page.evaluate(()=>fetch('/api/scene').then(r=>r.json()))
  const wait=async predicate=>{for(let i=0;i<600;i++){const s=await scene();if(predicate(s)&&await page.evaluate(rev=>document.querySelector('#status-rev').textContent===`Revision ${rev}`,s.revision))return s;await page.waitForTimeout(50)}throw new Error('Bezier edit did not settle')}
  const key=s=>s.objects[0].tracks[0].keys[0]
  await command([
    {op:'clear'},{op:'set_animation',start:1,end:25},
    {op:'add',name:'Bezier ball',primitive:{kind:'cube'}},
    {op:'set_keyframe',id:'Bezier ball',property:'translation',frame:1,value:[0,1,0],interpolation:'linear'},
    {op:'set_keyframe',id:'Bezier ball',property:'translation',frame:25,value:[2,1,0]},
    {op:'set_keyframe',id:'Bezier ball',property:'rotation',frame:1,value:[0,0,0],interpolation:'bezier'},
    {op:'set_keyframe',id:'Bezier ball',property:'rotation',frame:25,value:[0,Math.PI/2,0]}
  ])
  await page.click('#outliner li');await page.click('[data-curve-key="1"]')
  await page.selectOption('#curve-interpolation','bezier');await page.click('[data-curve-apply]')
  await wait(s=>key(s).interpolation==='bezier')
  check(await page.locator('[data-curve-handle=out]').count()===1,'Bezier interpolation exposes the outgoing time/value handle')
  const before=await scene()
  const dot=await page.locator('[data-curve-handle=out]').boundingBox(),svg=await page.locator('[data-curve-plot]').boundingBox()
  await page.mouse.move(dot.x+dot.width/2,dot.y+dot.height/2);await page.mouse.down()
  await page.mouse.move(dot.x+dot.width/2+svg.width*300/360*.15,dot.y+dot.height/2-svg.height*.2,{steps:8});await page.mouse.up()
  const shaped=await wait(s=>!!key(s).out_handle)
  check(shaped.revision===before.revision+1&&key(shaped).out_handle.time[0]>.45&&key(shaped).out_handle.value[0]>.5,'dragging a handle edits both time and value with one command')
  check(key(shaped).out_handle.time[1]===1/3&&key(shaped).out_handle.value.slice(1).every(v=>v===0),'handle edits preserve untouched vector components')
  await page.fill('[data-field=translation][data-i="0"]','0.25');await page.press('[data-field=translation][data-i="0"]','Enter')
  const autoKeyed=await wait(s=>key(s).value[0]===.25)
  check(key(autoKeyed).interpolation==='bezier'&&JSON.stringify(key(autoKeyed).out_handle)===JSON.stringify(key(shaped).out_handle),"auto-key updates retain an existing key's interpolation and precise handles")
  await page.click('[data-action=undo]');await wait(s=>key(s).value[0]===0)
  await page.click('[data-action=undo]');await wait(s=>!key(s).out_handle)
  check(true,'one Undo restores the default Bezier controls')
  await page.click('[data-action=redo]');await wait(s=>!!key(s).out_handle)
  await page.fill('#curve-frame','2');await page.click('[data-curve-apply]')
  const retimed=await wait(s=>key(s).frame===2)
  check(JSON.stringify(key(retimed).out_handle)===JSON.stringify(key(shaped).out_handle),'retiming a key preserves its relative handles')
  await page.click('[data-curve-reset=out]');await wait(s=>!key(s).out_handle)
  check(await page.inputValue('#curve-interpolation')==='bezier','resetting a handle retains Bezier interpolation')
  await command([
    {op:'edit_keyframe',id:'Bezier ball',property:'translation',frame:2,new_frame:1},
    {op:'set_key_handle',id:'Bezier ball',property:'translation',frame:1,side:'out',handle:{time:[.8,1/3,1/3],value:[2,0,0]}},
    {op:'set_key_handle',id:'Bezier ball',property:'translation',frame:25,side:'in',handle:{time:[.1,1/3,1/3],value:[0,0,0]}}
  ])
  // Rust frame packets and the local viewport sampler must agree at a fractional time.
  await page.evaluate(()=>window.__tatara.setFrame(19.3))
  const native=await page.evaluate(()=>fetch('/api/context?frame=19.3').then(r=>r.json()))
  const x=native.objects[0].pose.transform.translation[0]
  check(Math.abs(x-1.75)<1e-9,'Rust samples the time-inverted Bezier curve at fractional frames')
  const ballId=(await scene()).objects[0].id
  await page.waitForFunction(id=>Math.abs(window.__tatara.position(id)[0]-1.75)<1e-9,ballId)
  check(await page.evaluate(id=>Math.abs(window.__tatara.position(id)[0]-1.75)<1e-9,ballId),'the browser viewport agrees with Rust fractional Bezier sampling')
  // A curve SVG point is built with the same JS sampler as viewport playback.
  const points=await page.locator('.curve-line').getAttribute('d')
  check(points.includes('L')&&await page.locator('[data-curve-handle=out]').count()===1,'the authored graph plots the sampled curve and its controls')
  await page.click('[data-curve-key="25"]')
  check(await page.locator('[data-curve-handle=in]').count()===1,'the next key exposes the incoming handle of a Bezier segment')
  await page.selectOption('#curve-channel','1');await page.selectOption('#curve-component','1');await page.click('[data-curve-key="1"]')
  const angular=await page.locator('[data-curve-handle=out]').boundingBox(),plot=await page.locator('[data-curve-plot]').boundingBox()
  await page.mouse.move(angular.x+angular.width/2,angular.y+angular.height/2);await page.mouse.down();await page.mouse.move(angular.x+angular.width/2,angular.y+angular.height/2-plot.height*.12,{steps:6});await page.mouse.up()
  const rotated=await wait(s=>!!s.objects[0].tracks[1].keys[0].out_handle)
  check(rotated.objects[0].tracks[1].keys[0].out_handle.value[1]>.1&&rotated.objects[0].tracks[1].keys[0].out_handle.value[1]<1,'rotation handle offsets convert displayed degrees to stored radians')
  await command([
    {op:'set_keyframe',id:'Bezier ball',property:'color',frame:1,value:'#204060'},
    {op:'set_keyframe',id:'Bezier ball',property:'color',frame:25,value:'#ffffff'}
  ])
  await page.selectOption('#curve-channel','2');await page.selectOption('#curve-component','0');await page.click('[data-curve-key="1"]')
  await page.selectOption('#curve-interpolation','bezier');await page.click('[data-curve-apply]')
  const colored=await wait(s=>s.objects[0].tracks[2].keys[0].interpolation==='bezier')
  check(colored.objects[0].tracks[2].keys[0].value[0]===32/255,'changing a colour channel to Bezier preserves its exact sRGB key value')
  const colorControl=await page.locator('[data-curve-handle=out]').boundingBox(),colorPlot=await page.locator('[data-curve-plot]').boundingBox()
  await page.mouse.move(colorControl.x+colorControl.width/2,colorControl.y+colorControl.height/2);await page.mouse.down();await page.mouse.move(colorControl.x+colorControl.width/2,colorControl.y+colorControl.height/2-colorPlot.height*.1,{steps:5});await page.mouse.up()
  const colorShape=await wait(s=>!!s.objects[0].tracks[2].keys[0].out_handle)
  check(colorShape.objects[0].tracks[2].keys[0].out_handle.value[0]>0&&colorShape.objects[0].tracks[2].keys[0].value[0]===32/255,'colour handle edits leave the authored colour key intact')
  const cancelBefore=await scene()
  await page.evaluate(()=>{
    const control=document.querySelector('[data-curve-handle=out]'),r=control.getBoundingClientRect(),x=r.left+r.width/2,y=r.top+r.height/2
    control.dispatchEvent(new PointerEvent('pointerdown',{bubbles:true,pointerId:9,button:0,clientX:x,clientY:y}))
    document.dispatchEvent(new PointerEvent('pointermove',{pointerId:9,clientX:x+20,clientY:y-20}))
    document.dispatchEvent(new PointerEvent('pointercancel',{pointerId:9}))
  })
  check(JSON.stringify(await scene())===JSON.stringify(cancelBefore),'cancelling a handle drag does not submit an edit')
  await command([
    {op:'set_keyframe',id:'Bezier ball',property:'opacity',frame:1,value:.5,interpolation:'bezier'},
    {op:'set_keyframe',id:'Bezier ball',property:'opacity',frame:25,value:.5},
    {op:'set_key_handle',id:'Bezier ball',property:'opacity',frame:1,side:'out',handle:{time:[1/3],value:[100]}}
  ])
  await page.evaluate(()=>window.__tatara.setFrame(13))
  const bounded=await page.evaluate(()=>fetch('/api/context?frame=13').then(r=>r.json()))
  check(bounded.objects[0].pose.material.opacity===1,'Bezier overshoot keeps opacity within its physical range')

}
