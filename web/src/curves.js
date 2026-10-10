// Authored animation curves. Each key edit is one typed, collision-checked command.
import { sampleTrack, isColor } from './anim.js'
const W=360,H=220,L=44,R=16,T=16,B=32
const escape=value=>String(value).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]))
const fmt=value=>String(Math.round(value*1000)/1000)
const angular=track=>track.property==='rotation'||track.property==='bone'
const label=track=>track.property==='bone'?`${track.bone} rotation`:track.property.replaceAll('_',' ')

export class CurveEditor {
  constructor({run,setFrame,onError}) {
    Object.assign(this,{run,setFrame,onError})
    this.channel=0;this.component=0;this.selected=null
    document.addEventListener('pointerdown',e=>this.down(e))
    document.addEventListener('pointermove',e=>this.move(e))
    document.addEventListener('pointerup',e=>this.up(e))
    document.addEventListener('pointercancel',()=>{if(this.drag){this.drag=null;this.draw(true)}})
    document.addEventListener('change',e=>{
      if(!e.target.closest('#curve-editor'))return
      if(e.target.id==='curve-channel'){this.channel=Number(e.target.value);this.component=0;this.selected=null;this.draw(true)}
      if(e.target.id==='curve-component'){this.component=Number(e.target.value);this.draw(true)}
    })
    document.addEventListener('click',e=>{if(e.target.closest('[data-curve-apply]'))this.applyFields();const reset=e.target.closest('[data-curve-reset]');if(reset&&!this.playing)this.commitHandle(this.track.keys.find(k=>k.frame===this.selected),reset.dataset.curveReset,null)})
    document.addEventListener('keydown',e=>{
      const key=e.target.closest('[data-curve-key]')
      if(key&&(e.key==='Enter'||e.key===' ')){e.preventDefault();this.selected=Number(key.dataset.curveKey);this.draw(true);this.setFrame(this.selected);document.querySelector('#curve-frame')?.focus()}
      if(e.key==='Enter'&&e.target.matches('#curve-frame,#curve-value')){e.preventDefault();this.applyFields()}
    })
  }
  render(object,frame,range,playing) {
    this.object=object;this.frame=frame;this.range=range;this.playing=playing
    if(!object){document.querySelector('#curve-editor')?.remove();return}
    let root=document.querySelector('#curve-editor')
    if(!root){root=document.createElement('section');root.id='curve-editor';root.className='card';document.querySelector('#properties').append(root)}
    const fresh=this.root!==root||this.id!==object.id
    this.root=root
    if(this.id!==object.id){this.channel=0;this.component=0;this.selected=null;this.drag=null}
    this.id=object.id
    this.draw(fresh)
  }
  draw(force=false) {
    if(!this.root||this.drag)return
    const tracks=this.object.tracks??[]
    this.channel=Math.min(this.channel,Math.max(0,tracks.length-1))
    const track=tracks[this.channel]
    if(!track){this.root.innerHTML='<div class="card-title">Animation curves</div><p class="muted small">Press Key to add animation, then edit its curves here.</p>';this.signature='';return}
    this.track=track
    this.component=Math.min(this.component,track.keys[0].value.length-1)
    if(!track.keys.some(k=>k.frame===this.selected))this.selected=track.keys[0].frame
    const signature=JSON.stringify([this.id,tracks,this.channel,this.component,this.selected,this.range,this.playing])
    if(!force&&signature===this.signature){this.playhead();return}
    this.signature=signature
    this.factor=angular(track)?180/Math.PI:1
    const values=track.keys.map(k=>k.value[this.component]*this.factor)
    for(let i=0;i<track.keys.length-1;i++)if(track.keys[i].interpolation==='bezier'){
      values.push((track.keys[i].value[this.component]+(track.keys[i].out_handle?.value[this.component]??0))*this.factor)
      values.push((track.keys[i+1].value[this.component]+(track.keys[i+1].in_handle?.value[this.component]??0))*this.factor)
    }
    const min=Math.min(...values),max=Math.max(...values),pad=Math.max((max-min)*.15,.25)
    this.scale={x0:Math.min(this.range.start,track.keys[0].frame),x1:Math.max(this.range.end,track.keys.at(-1).frame),y0:min-pad,y1:max+pad}
    if(this.scale.x1===this.scale.x0)this.scale.x1++
    const {x0,x1,y0,y1}=this.scale
    const ticks=[0,.5,1].map(t=>{
      const x=L+t*(W-L-R),y=T+t*(H-T-B)
      return `<path class="curve-grid" d="M${x},${T}V${H-B}M${L},${y}H${W-R}"/><text x="${x}" y="${H-8}" text-anchor="middle">${fmt(x0+t*(x1-x0))}</text><text x="${L-6}" y="${y+4}" text-anchor="end">${fmt(y1-t*(y1-y0))}</text>`
    }).join('')
    const path=this.pathFor(track)
    const selected=track.keys.find(k=>k.frame===this.selected)
    const disabled=this.playing?'disabled':''
    const index=track.keys.indexOf(selected),handles=[]
    if(index>0&&track.keys[index-1].interpolation==='bezier')handles.push(this.handlePoint(selected,'in',track.keys[index-1]))
    if(index<track.keys.length-1&&selected.interpolation==='bezier')handles.push(this.handlePoint(selected,'out',track.keys[index+1]))
    const controls=handles.map(h=>`<line class="curve-handle-line" x1="${this.x(selected.frame)}" y1="${this.y(selected.value[this.component]*this.factor)}" x2="${this.x(h.frame)}" y2="${this.y(h.value)}"/><circle class="curve-handle" data-curve-handle="${h.side}" cx="${this.x(h.frame)}" cy="${this.y(h.value)}" r="5" aria-label="${h.side} Bezier handle"/>`).join('')
    this.root.innerHTML=`<div class="card-title">Animation curves <span class="muted small">authored keys</span></div>
      <label class="row">Channel<select id="curve-channel">${tracks.map((t,i)=>`<option value="${i}" ${i===this.channel?'selected':''}>${escape(label(t))}</option>`).join('')}</select></label>
      <label class="row">Component<select id="curve-component">${selected.value.map((_,i)=>`<option value="${i}" ${i===this.component?'selected':''}>${selected.value.length===1?'Value':['X','Y','Z'][i]}</option>`).join('')}</select></label>
      <svg data-curve-plot viewBox="0 0 ${W} ${H}" data-x0="${x0}" data-x1="${x1}" data-y0="${y0}" data-y1="${y1}" aria-label="Authored animation curve">${ticks}<path class="curve-line" d="${path}"/><line class="curve-playhead" y1="${T}" y2="${H-B}"/>
      ${controls}${track.keys.map(k=>`<circle class="curve-key ${k.frame===this.selected?'selected':''}" data-curve-key="${k.frame}" cx="${this.x(k.frame)}" cy="${this.y(k.value[this.component]*this.factor)}" r="5" tabindex="0" role="button" aria-label="Key frame ${k.frame}, value ${fmt(k.value[this.component]*this.factor)}"/>`).join('')}</svg>
      <div class="curve-fields"><label>Frame<input id="curve-frame" type="number" min="0" max="100000" step=".01" value="${selected.frame}" ${disabled}></label><label>Value${angular(track)?'°':''}<input id="curve-value" type="number" step=".01" value="${fmt(selected.value[this.component]*this.factor)}" ${disabled}></label></div>
      <div class="row"><select id="curve-interpolation" aria-label="Curve interpolation" ${disabled}>${['ease','linear','step','bezier'].map(v=>`<option ${selected.interpolation===v?'selected':''} value="${v}">${v[0].toUpperCase()+v.slice(1)}</option>`).join('')}</select><button class="small-btn" data-curve-apply ${disabled}>Apply</button></div>
      ${handles.length?`<div class="row">${handles.map(h=>`<button class="small-btn" data-curve-reset="${h.side}" ${disabled}>Reset ${h.side} handle</button>`).join('')}</div>`:''}
      <p class="muted small">${this.playing?'Pause playback to edit keys.':'Drag keys or Bezier handles. One edit, one Undo. The left key controls interpolation.'}</p>`
    this.playhead()
  }
  pathFor(track){
    const {x0,x1}=this.scale
    const points=[[x0,track.keys[0].value[this.component]*this.factor]]
    for(let i=0;i<track.keys.length-1;i++){
      const a=track.keys[i],b=track.keys[i+1]
      if(a.interpolation==='step')points.push([b.frame,a.value[this.component]*this.factor])
      else for(let j=1;j<16;j++){const f=a.frame+(b.frame-a.frame)*j/16;points.push([f,sampleTrack(track,f)[this.component]*this.factor])}
      points.push([b.frame,b.value[this.component]*this.factor])
    }
    points.push([x1,track.keys.at(-1).value[this.component]*this.factor])
    return points.map(([f,v],i)=>`${i?'L':'M'}${this.x(f)},${this.y(v)}`).join(' ')
  }
  handlePoint(key,side,neighbor){
    const handle=key[`${side}_handle`],k=this.component
    return {side,frame:key.frame+(neighbor.frame-key.frame)*(handle?.time[k]??1/3),value:(key.value[k]+(handle?.value[k]??0))*this.factor,neighbor}
  }
  async commitHandle(key,side,handle){
    if(!key)return
    try{await this.run([{op:'set_key_handle',id:this.id,property:this.track.property,...(this.track.bone?{bone:this.track.bone}:{}),frame:key.frame,side,handle}])}catch{ /* run reports command errors without changing the authored scene. */ }finally{this.draw(true)}
  }
  x(frame){return L+(frame-this.scale.x0)/(this.scale.x1-this.scale.x0)*(W-L-R)}
  y(value){return T+(this.scale.y1-value)/(this.scale.y1-this.scale.y0)*(H-T-B)}
  playhead(){const line=this.root.querySelector('.curve-playhead');if(line){const x=this.x(this.frame);line.setAttribute('x1',x);line.setAttribute('x2',x)}}
  async commit(old,newFrame,value,interpolation) {
    try{
      const changed=value.some((v,i)=>v!==old.value[i])
      let payload=value.length===1?value[0]:value
      if(changed&&isColor(this.track.property)&&this.track.property!=='light_color')payload=`#${value.map(v=>Math.round(Math.max(0,Math.min(1,v))*255).toString(16).padStart(2,'0')).join('')}`
      await this.run([{op:'edit_keyframe',id:this.id,property:this.track.property,...(this.track.bone?{bone:this.track.bone}:{}),frame:old.frame,new_frame:newFrame,...(changed?{value:payload}:{}),interpolation}])
      this.selected=newFrame
    }catch(error){this.selected=old.frame}
    this.draw(true)
  }
  applyFields(){
    const old=this.track?.keys.find(k=>k.frame===this.selected)
    if(!old||this.playing)return
    const frame=Number(this.root.querySelector('#curve-frame').value),value=[...old.value]
    const input=this.root.querySelector('#curve-value').value
    if(!input.trim()||!this.root.querySelector('#curve-frame').value.trim())return this.onError('Frame and value are required')
    if(input!==fmt(old.value[this.component]*this.factor))value[this.component]=Number(input)/this.factor
    if(!Number.isFinite(frame)||value.some(v=>!Number.isFinite(v)))return this.onError('Frame and value must be finite numbers')
    this.commit(old,frame,value,this.root.querySelector('#curve-interpolation').value)
  }
  down(e){
    const control=e.target.closest('[data-curve-handle]')
    if(control&&!this.playing&&e.button===0){
      const old=this.track.keys.find(k=>k.frame===this.selected),side=control.dataset.curveHandle,index=this.track.keys.indexOf(old)
      const neighbor=this.track.keys[index+(side==='in'?-1:1)]
      if(!neighbor)return
      this.drag={old:structuredClone(old),side,neighbor,circle:control,rect:this.root.querySelector('svg').getBoundingClientRect(),start:[e.clientX,e.clientY],handle:structuredClone(old[`${side}_handle`]??{time:old.value.map(()=>1/3),value:old.value.map(()=>0)}),moved:false,pointer:e.pointerId}
      if(e.isTrusted)control.setPointerCapture(e.pointerId)
      e.preventDefault();return
    }
    const element=e.target.closest('[data-curve-key]')
    if(!element||this.playing||e.button!==0)return
    const old=this.track.keys.find(k=>k.frame===Number(element.dataset.curveKey))
    this.selected=old.frame;this.draw(true)
    const circle=this.root.querySelector(`[data-curve-key="${old.frame}"]`),svg=this.root.querySelector('svg')
    this.drag={old:structuredClone(old),circle,rect:svg.getBoundingClientRect(),start:[e.clientX,e.clientY],frame:old.frame,value:[...old.value],moved:false,pointer:e.pointerId}
    if(e.isTrusted)circle.setPointerCapture(e.pointerId)
    e.preventDefault()
  }
  move(e){
    const drag=this.drag;if(!drag||e.pointerId!==drag.pointer)return
    if(Math.hypot(e.clientX-drag.start[0],e.clientY-drag.start[1])<3&&!drag.moved)return
    drag.moved=true
    const x=(e.clientX-drag.rect.left)/drag.rect.width*W,y=(e.clientY-drag.rect.top)/drag.rect.height*H
    if(drag.side){
      const frame=this.scale.x0+(x-L)/(W-L-R)*(this.scale.x1-this.scale.x0)
      drag.handle.time[this.component]=Math.max(0,Math.min(1,(frame-drag.old.frame)/(drag.neighbor.frame-drag.old.frame)))
      const value=(this.scale.y1-(y-T)/(H-T-B)*(this.scale.y1-this.scale.y0))/this.factor
      drag.handle.value[this.component]=Math.max(-1e6,Math.min(1e6,value-drag.old.value[this.component]))
      const point=this.handlePoint({...drag.old,[`${drag.side}_handle`]:drag.handle},drag.side,drag.neighbor)
      drag.circle.setAttribute('cx',this.x(point.frame));drag.circle.setAttribute('cy',this.y(point.value))
      // Update the line associated with this control without rebuilding the active pointer target.
      const preview=structuredClone(this.track)
      preview.keys.find(k=>k.frame===drag.old.frame)[`${drag.side}_handle`]=drag.handle
      this.root.querySelector('.curve-line').setAttribute('d',this.pathFor(preview))
      const stem=drag.circle.previousElementSibling
      if(stem?.classList.contains('curve-handle-line')){stem.setAttribute('x2',this.x(point.frame));stem.setAttribute('y2',this.y(point.value))}
      return
    }
    drag.frame=Math.max(0,Math.min(100000,Math.round((this.scale.x0+(x-L)/(W-L-R)*(this.scale.x1-this.scale.x0))*100)/100))
    if(Math.abs(e.clientY-drag.start[1])>=1)drag.value[this.component]=Math.round((this.scale.y1-(y-T)/(H-T-B)*(this.scale.y1-this.scale.y0))/this.factor*10000)/10000
    drag.circle.setAttribute('cx',this.x(drag.frame));drag.circle.setAttribute('cy',this.y(drag.value[this.component]*this.factor))
    this.root.querySelector('#curve-frame').value=drag.frame;this.root.querySelector('#curve-value').value=fmt(drag.value[this.component]*this.factor)
  }
  up(e){const drag=this.drag;if(!drag||e.pointerId!==drag.pointer)return;this.drag=null;if(drag.moved){if(drag.side)this.commitHandle(drag.old,drag.side,drag.handle);else this.commit(drag.old,drag.frame,drag.value,drag.old.interpolation);}else{this.draw(true);this.setFrame(drag.old.frame)}}
}
