// 16px stroke icons.
const P = {
  cube: '<path d="M8 1.8 13.6 5v6L8 14.2 2.4 11V5z"/><path d="M2.4 5 8 8.2 13.6 5M8 8.2v6"/>',
  sphere: '<circle cx="8" cy="8" r="6"/><ellipse cx="8" cy="8" rx="6" ry="2.3"/><path d="M8 2c-2.2 2.4-2.2 9.6 0 12"/>',
  cylinder: '<ellipse cx="8" cy="3.8" rx="5" ry="1.8"/><path d="M3 3.8v8.4c0 1 2.2 1.8 5 1.8s5-.8 5-1.8V3.8"/>',
  torus: '<ellipse cx="8" cy="8" rx="6.2" ry="3.6"/><ellipse cx="8" cy="7.6" rx="2.6" ry="1.1"/>',
  vessel: '<path d="M6 1.8h4M6.4 1.8c0 2-.3 2.6-1.8 4.1C3.1 7.4 3 9.6 3.8 11.6c.6 1.6 2.1 2.6 4.2 2.6s3.6-1 4.2-2.6c.8-2 .7-4.2-.8-5.7C9.9 4.4 9.6 3.8 9.6 1.8"/>',
  plane: '<path d="M1.6 10.4 8 13.6l6.4-3.2L8 7.2z"/>',
  extrude: '<path d="M2.5 10.5 8 13.5l5.5-3L8 7.5z"/><path d="M8 7.5V1.8M5.6 4.2 8 1.8l2.4 2.4"/>',
  subdivide: '<rect x="2.2" y="2.2" width="11.6" height="11.6" rx="3.4"/><path d="M8 2.2v11.6M2.2 8h11.6"/>',
  duplicate: '<rect x="5" y="5" width="8.6" height="8.6" rx="1.6"/><path d="M11 5V3.6C11 2.7 10.3 2 9.4 2H3.6C2.7 2 2 2.7 2 3.6v5.8c0 .9.7 1.6 1.6 1.6H5"/>',
  delete: '<path d="M2.8 4.2h10.4M6.2 4.2V2.6h3.6v1.6M4 4.2l.7 9.2h6.6l.7-9.2"/>',
  undo: '<path d="M5.4 3 2.4 6l3 3"/><path d="M2.6 6h7a4 4 0 0 1 0 8H6"/>',
  redo: '<path d="M10.6 3l3 3-3 3"/><path d="M13.4 6h-7a4 4 0 0 0 0 8H10"/>',
  open: '<path d="M1.8 4.2c0-.8.6-1.4 1.4-1.4h3l1.4 1.6h5.2c.8 0 1.4.6 1.4 1.4v6.6c0 .8-.6 1.4-1.4 1.4H3.2c-.8 0-1.4-.6-1.4-1.4z"/>',
  save: '<path d="M8 2v8M4.8 7 8 10.2 11.2 7"/><path d="M2.6 11v1.6c0 .8.6 1.4 1.4 1.4h8c.8 0 1.4-.6 1.4-1.4V11"/>',
  exportObj: '<path d="M8 10V2M4.8 5 8 1.8 11.2 5"/><path d="M2.6 9.4v3.2c0 .8.6 1.4 1.4 1.4h8c.8 0 1.4-.6 1.4-1.4V9.4"/>',
  exportGlb: '<path d="M8 1.6 13.6 4.8v6.4L8 14.4 2.4 11.2V4.8z"/><path d="M8 5v5.2M5.8 8 8 10.2 10.2 8"/>',
  wire: '<path d="M8 1.8 13.6 5v6L8 14.2 2.4 11V5z"/><path d="M2.4 5 8 8.2 13.6 5M8 8.2v6M2.4 11 13.6 5M2.4 5l11.2 6" opacity=".55"/>',
  frame: '<path d="M2 5.4V2h3.4M10.6 2H14v3.4M14 10.6V14h-3.4M5.4 14H2v-3.4"/><circle cx="8" cy="8" r="2.2"/>',
  inset: '<rect x="2" y="2" width="12" height="12" rx="1.5"/><rect x="5.2" y="5.2" width="5.6" height="5.6" rx=".8"/><path d="M2 2l3.2 3.2M14 2l-3.2 3.2M2 14l3.2-3.2M14 14l-3.2-3.2" opacity=".6"/>',
  'mod-mirror': '<path d="M8 1.5v13" stroke-dasharray="1.6 1.6"/><path d="M6 4 2.5 12H6zM10 4l3.5 8H10z"/>',
  'mod-subdivision': '<rect x="2.2" y="2.2" width="11.6" height="11.6" rx="4.5"/><path d="M8 2.2v11.6M2.2 8h11.6" opacity=".55"/>',
  'mod-array': '<rect x="1.6" y="9.6" width="4" height="4" rx=".8"/><rect x="6" y="5.8" width="4" height="4" rx=".8"/><rect x="10.4" y="2" width="4" height="4" rx=".8"/>',
  'mod-twist': '<path d="M4 2c5 2 3 4 8 4M4 6c5 2 3 4 8 4M4 10c5 2 3 4 8 4"/>',
  'mod-taper': '<path d="M3 14h10L10 2H6z"/>',
  bevel: '<path d="M2 14V6l4-4h8"/><path d="M2 6h4V2" opacity=".55"/>',
  loopcut: '<rect x="2" y="2" width="12" height="12" rx="1.5"/><path d="M8 2v12" stroke-dasharray="2 1.6"/>',
  object: '<path d="M8 1.8 13.6 5v6L8 14.2 2.4 11V5z"/>',
  edit: '<path d="M8 1.8 13.6 5v6L8 14.2 2.4 11V5z" opacity=".5"/><circle cx="8" cy="1.8" r="1.2" fill="currentColor"/><circle cx="13.6" cy="5" r="1.2" fill="currentColor"/><circle cx="2.4" cy="5" r="1.2" fill="currentColor"/><circle cx="8" cy="8.2" r="1.2" fill="currentColor"/>',
  'sel-vertex': '<rect x="2.5" y="2.5" width="11" height="11" rx="1" opacity=".45"/><circle cx="2.5" cy="2.5" r="1.8" fill="currentColor"/><circle cx="13.5" cy="13.5" r="1.8" fill="currentColor"/>',
  'sel-edge': '<rect x="2.5" y="2.5" width="11" height="11" rx="1" opacity=".45"/><path d="M2.5 13.5h11" stroke-width="2.6"/>',
  'sel-face': '<rect x="2.5" y="2.5" width="11" height="11" rx="1" fill="currentColor" fill-opacity=".45"/>',
  play: '<path d="M4.5 2.8v10.4L13 8z" fill="currentColor"/>',
  pause: '<path d="M4.5 3v10M11.5 3v10" stroke-width="2.4"/>',
  cursor: '<path d="M3.5 2.5 12.5 8l-4 1-2 4.5z"/>',
}

export function icon(name) {
  const body = P[name]
  if (!body) return ''
  return `<svg class="icon" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.35" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${body}</svg>`
}
