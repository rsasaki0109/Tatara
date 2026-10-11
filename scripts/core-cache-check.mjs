// Real HTTP caching across documents must not keep a prior Rust core.
import fs from 'node:fs'
import http from 'node:http'
import path from 'node:path'
import {fileURLToPath} from 'node:url'

const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..')

export async function checkCoreCache(browser, wasmPath, check) {
  const fresh=fs.readFileSync(wasmPath), prior=Buffer.from(fresh)
  const needle=Buffer.from('tatara-project'), marker=Buffer.from('cached-project')
  let replaced=0
  // Equal-length data changes retain the real ABI while identifying an older core.
  for(let i=prior.indexOf(needle);i>=0;i=prior.indexOf(needle,i+needle.length)){marker.copy(prior,i);replaced++}
  if(!replaced)throw new Error('Core cache fixture needs its project format marker')
  const backend=fs.readFileSync(path.join(root,'web/src/backend.js'),'utf8')
  let updated=false,notModified=0,apiRequests=0
  const server=http.createServer((req,res)=>{
    const pathname=new URL(req.url,'http://localhost').pathname
    if(pathname.startsWith('/api/'))apiRequests++
    if(pathname==='/tatara.wasm'){
      const etag=updated?'"current-core"':'"prior-core"'
      res.setHeader('Cache-Control','public, max-age=3600')
      res.setHeader('ETag',etag)
      if(req.headers['if-none-match']===etag){notModified++;res.writeHead(304);return res.end()}
      res.setHeader('Content-Type','application/wasm')
      return res.end(updated?fresh:prior)
    }
    res.setHeader('Cache-Control','no-store')
    if(pathname==='/backend.js'){res.setHeader('Content-Type','text/javascript');return res.end(backend)}
    res.setHeader('Content-Type','text/html')
    res.end('<script type="module">import {installWasmBackend} from "/backend.js";await installWasmBackend("/tatara.wasm");window.coreReady=true;</script>')
  })
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve))
  const context=await browser.newContext(),errors=[]
  try {
    const page=await context.newPage()
    page.on('pageerror',e=>errors.push(e.message))
    const url=`http://127.0.0.1:${server.address().port}`
    async function visit(build){
      await page.goto(`${url}/?build=${build}`,{waitUntil:'domcontentloaded'})
      await page.waitForFunction(()=>window.coreReady===true)
      return page.evaluate(()=>fetch('/api/project').then(r=>r.json()).then(p=>p.format))
    }
    check(await visit(1)==='cached-project','the core-cache control loads the prior real WASM module')
    updated=true
    check(await visit(2)==='tatara-project','a normal revisit uses the updated Rust core instead of a fresh cached prior version')
    check(await visit(3)==='tatara-project'&&notModified>0,'an unchanged Rust core revalidates with 304 and keeps its project API')
    check(apiRequests===0&&errors.length===0,'core upgrade and revalidation use WASM without server API requests or page errors')
  } finally {await context.close();await new Promise(resolve=>server.close(resolve))}
}
