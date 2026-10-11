// Native and WASM share the same field-aware stale command contract.
export async function checkProperties(page, check) {
  const command = async body => page.evaluate(async body => {
    const response = await fetch('/api/commands', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)})
    return {status:response.status,data:await response.json()}
  },body)
  const scene = () => page.evaluate(()=>fetch('/api/scene').then(response=>response.json()))
  await command({commands:[{op:'clear'},{op:'add',name:'Shared box',primitive:{kind:'cube'}}]})
  const initial=await scene()
  const first=await command({expected_revision:initial.revision,actor:{id:'alice',name:'Alice'},commands:[{op:'move',id:'Shared box',offset:[2,0,0]}]})
  const second=await command({expected_revision:initial.revision,rebase:true,actor:{id:'bob',name:'Bob'},commands:[{op:'material',id:'Shared box',color:'#ff8800'}]})
  const combined=await scene()
  check(first.status===200 && second.status===200 && second.data.rebased_from===initial.revision,'same-object independent properties combine from an older revision')
  check(combined.objects[0].transform.translation[0]===2 && combined.objects[0].material.color==='#ff8800','a property rebase retains both movement and color')
  const history=await page.evaluate(()=>fetch('/api/history').then(response=>response.json()))
  check(history.steps.at(-2).actor.name==='Alice' && history.steps.at(-1).actor.name==='Bob' && history.steps.at(-1).rebased_from===initial.revision,'combined same-object edits retain both authors in history')
  const conflict=await command({expected_revision:first.data.revision,rebase:true,commands:[{op:'material',id:'Shared box',color:'#ffffff'}]})
  check(conflict.status===409 && conflict.data.detail.stale && JSON.stringify(await scene())===JSON.stringify(combined),'same-property overwrites are rejected atomically with a recovery reason')
  await page.evaluate(()=>window.__tatara.refresh(false))
  await page.click('[data-action=undo]')
  for(let i=0;i<600;i++) {const state=await scene();if(state.objects[0]?.material.color!=='#ff8800' && state.objects[0]?.transform.translation[0]===2)break;await page.waitForTimeout(50);if(i===599)throw new Error('property rebase Undo did not settle')}
  check(true,'one UI Undo removes the rebased property and retains the prior movement')
  await page.click('[data-action=redo]')
  for(let i=0;i<600;i++) {const state=await scene();if(state.objects[0]?.material.color==='#ff8800' && state.objects[0]?.transform.translation[0]===2)break;await page.waitForTimeout(50);if(i===599)throw new Error('property rebase Redo did not settle')}
  check(true,'UI Redo restores the combined same-object state')
}

// Both edits originate from real inspector controls in separate shared browsers.
export async function checkPeerProperties(alice, bob, until, check) {
  await alice.evaluate(async()=>{const response=await fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({commands:[{op:'clear'},{op:'add',name:'Shared sculpture',primitive:{kind:'cube'}}]})});if(!response.ok)throw new Error(await response.text())})
  for(const page of [alice,bob]) {
    await until(page,()=>document.querySelector('#outliner').textContent.includes('Shared sculpture'))
    await page.click('.tabs [data-tab=properties]')
    await page.click('#outliner li:first-child')
  }
  const context=await alice.evaluate(()=>fetch('/api/scene').then(response=>response.json()).then(scene=>scene.revision))
  const position='[data-field=translation][data-i="0"]'
  await alice.fill(position,'2')
  await bob.click('[data-preset=gold]')
  await until(bob,()=>fetch('/api/scene').then(response=>response.json()).then(scene=>scene.objects[0].material.metalness===1))
  await alice.press(position,'Tab')
  await until(alice,()=>fetch('/api/scene').then(response=>response.json()).then(scene=>scene.objects[0].transform.translation[0]===2 && scene.objects[0].material.metalness===1))
  const history=await alice.evaluate(()=>fetch('/api/history').then(response=>response.json()))
  check(history.steps.at(-1).rebased_from===context && history.steps.at(-1).actor.name==='Alice' && history.steps.at(-2).actor.name==='Bob','two browsers combine a focused position edit with a material preset on the same object')
  await until(bob,()=>document.querySelector('[data-field=translation][data-i="0"]')?.value==='2')
  check(true,'both browser inspectors display the combined properties')
  await alice.click('[data-action=undo]')
  await until(bob,()=>fetch('/api/scene').then(response=>response.json()).then(scene=>scene.objects[0].transform.translation[0]===0 && scene.objects[0].material.metalness===1))
  check(true,'shared Undo removes only the later position edit and preserves the other participant material')
}
