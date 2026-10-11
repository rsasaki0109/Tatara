// Connected vertex merging has the same command and UI contract in both cores.
export async function checkMerge(page, check) {
  const commands = body => page.evaluate(async body => {
    const response = await fetch('/api/commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)})
    return {status:response.status,data:await response.json()}
  },body)
  const scene = () => page.evaluate(()=>fetch('/api/scene').then(r=>r.json()))
  const wait = async predicate => {for(let i=0;i<600;i++){const s=await scene();if(predicate(s))return s;await page.waitForTimeout(50)}throw new Error('vertex merge did not settle')}
  const setup = async () => {
    const r=await commands({commands:[{op:'clear'},{op:'add',name:'Merge box',primitive:{kind:'cube',size:2}}]})
    if(r.status!==200)throw new Error(JSON.stringify(r))
    await page.evaluate(()=>window.__tatara.refresh(false))
    await page.click('.tabs [data-tab=properties]')
    await page.click('#outliner li:first-child')
  }
  await setup()
  await page.click('[data-mode=edit]');await page.click('[data-select-mode=vertex]')
  check(await page.isDisabled('[data-action=mergeVertices]'),'Merge is disabled without two selected vertices')
  await page.keyboard.press('a')
  check(!await page.isDisabled('[data-action=mergeVertices]'),'vertex selection enables the Merge control')
  const before=await scene()
  await page.keyboard.press('m')
  for(let i=0;i<600;i++){if((await page.locator('#toasts').textContent()).includes('remove every face'))break;await page.waitForTimeout(50);if(i===599)throw new Error('merge error not shown')}
  check(JSON.stringify(await scene())===JSON.stringify(before),'M rejects collapsing the entire mesh with a visible reason and no changes')
  await page.click('[data-mode=object]')
  for(const [at,expected] of [['center',[0,1,0]],['first',[-1,1,1]],['last',[-1,1,-1]]]) {
    await setup()
    const r=await commands({commands:[{op:'merge_vertices',id:'Merge box',vertices:[7,6,2,3],at}]})
    const result=await scene()
    check(r.status===200 && result.objects[0].mesh.vertices.length===5 && result.objects[0].mesh.faces.length===5 && JSON.stringify(result.objects[0].mesh.vertices[2])===JSON.stringify(expected),`Merge at ${at} produces the expected five-vertex pyramid`)
  }
  await page.evaluate(()=>window.__tatara.refresh(false))
  await page.click('[data-action=undo]');await wait(s=>s.objects[0].mesh.vertices.length===8)
  await page.click('[data-action=redo]');await wait(s=>s.objects[0].mesh.vertices.length===5)
  check(true,'UI Undo and Redo restore the original and merged topology')
  const current=await scene()
  const invalid=await commands({commands:[{op:'move',id:'Merge box',offset:[1,0,0]},{op:'merge_vertices',id:'Merge box',vertices:[0,99]}]})
  check(invalid.status===422 && JSON.stringify(await scene())===JSON.stringify(current),'an invalid vertex rejects the entire batch including its preceding movement')
}
