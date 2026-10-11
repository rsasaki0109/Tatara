// Exercise the same typed material and inspector controls in native and WASM.
export async function checkPackedMaterials(page, check) {
  const send = body => page.evaluate(async body => {
    const response = await fetch('/api/commands', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)})
    return {status:response.status,data:await response.json()}
  }, body)
  const scene = () => page.evaluate(()=>fetch('/api/scene').then(r=>r.json()))
  const data = await page.evaluate(()=>{
    const canvas=document.createElement('canvas');canvas.width=2;canvas.height=2
    const ctx=canvas.getContext('2d');ctx.fillStyle='rgb(255,64,192)';ctx.fillRect(0,0,2,2)
    return canvas.toDataURL('image/png').split(',')[1]
  })
  const setup=await send({commands:[{op:'clear'},{op:'add',name:'Packed sphere',primitive:{kind:'sphere'}},{op:'add_image',name:'Packed',data},{op:'material',id:'Packed sphere',roughness:0.4,metalness:0.8}]})
  if(setup.status!==200)throw new Error(JSON.stringify(setup))
  await page.evaluate(()=>window.__tatara.refresh(false))
  await page.click('.tabs [data-tab=properties]');await page.click('#outliner li:first-child')
  await page.selectOption('#p-metallic-roughness-map','Packed')
  let applied
  for(let i=0;i<600;i++){
    applied=await scene()
    if(applied.objects[0]?.material.texture?.metallic_roughness_map==='Packed')break
    await page.waitForTimeout(50)
    if(i===599)throw new Error('packed map inspector edit did not settle')
  }
  check(applied.objects[0].material.roughness===0.4 && applied.objects[0].material.metalness===0.8,'the packed map inspector retains the scalar material factors')
  let gpu
  for(let i=0;i<600;i++){
    gpu=await page.evaluate(()=>window.__tatara.debug().packedMaterials[0])
    if(gpu)break
    await page.waitForTimeout(50)
    if(i===599)throw new Error('packed GPU texture did not load')
  }
  check(gpu.roughness===0.4 && gpu.metalness===0.8 && gpu.colorSpace==='','the real Three.js material multiplies raw linear map channels by scalar factors')
  const invalid=await send({commands:[{op:'move',id:'Packed sphere',offset:[1,0,0]},{op:'material',id:'Packed sphere',texture:{pattern:'none',metallic_roughness_map:'Missing'}}]})
  check(invalid.status===422 && JSON.stringify(await scene())===JSON.stringify(applied),'a missing packed image rejects the entire material batch atomically')
  const remove=await send({commands:[{op:'delete_image',name:'Packed'}]})
  check(remove.status===422,'a packed material protects its referenced image from deletion')
  await page.click('[data-action=undo]')
  for(let i=0;i<600;i++){if(!(await scene()).objects[0].material.texture)break;await page.waitForTimeout(50);if(i===599)throw new Error('packed material Undo did not settle')}
  check(true,'one inspector Undo removes the packed map without removing its image')
  await page.click('[data-action=redo]')
  for(let i=0;i<600;i++){if((await scene()).objects[0].material.texture?.metallic_roughness_map==='Packed')break;await page.waitForTimeout(50);if(i===599)throw new Error('packed material Redo did not settle')}
  check(true,'Redo restores the packed material map')
}
