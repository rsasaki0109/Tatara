// Keyframe sampling for the viewport. Mirrors src/anim.rs so playback in the
// browser matches what the Rust renderer and glTF export produce.

export const PROPERTIES = ['translation', 'rotation', 'scale', 'color', 'roughness', 'metalness', 'emissive', 'emissive_strength', 'opacity']
export const CAMERA_PROPERTIES = ['translation', 'rotation', 'scale', 'camera_fov', 'camera_aperture', 'camera_focus', 'camera_height']
export const LIGHT_PROPERTIES = ['translation', 'rotation', 'scale', 'light_color', 'light_intensity']
export const propertiesFor = (o) => o.camera ? CAMERA_PROPERTIES : o.light ? LIGHT_PROPERTIES : PROPERTIES
const COLORS = ['color', 'emissive', 'light_color']
const RANGE = { emissive_strength: 20 }

function hexToRgb(hex) {
  return [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
}

function rgbToHex(rgb) {
  return `#${rgb.map((c) => Math.round(Math.min(1, Math.max(0, c)) * 255).toString(16).padStart(2, '0')).join('')}`
}

export function sampleTrack(track, frame) {
  const keys = track.keys
  if (frame <= keys[0].frame) return keys[0].value
  const last = keys[keys.length - 1]
  if (frame >= last.frame) return last.value
  let i = 0
  while (keys[i + 1].frame <= frame) i++
  const a = keys[i]
  const b = keys[i + 1]
  let t = (frame - a.frame) / (b.frame - a.frame)
  if (a.interpolation === 'step') t = 0
  else if (a.interpolation !== 'linear') t = t * t * (3 - 2 * t)
  return a.value.map((x, k) => x + (b.value[k] - x) * t)
}

export function restValue(o, property) {
  if (property === 'camera_height') return [o.camera.ortho_height ?? 4]
  if (property.startsWith('camera_')) return [o.camera[property.slice(7)]]
  if (property === 'light_color') return hexToRgb(o.light.color)
  if (property === 'light_intensity') return [o.light.intensity]
  switch (property) {
    case 'translation':
    case 'rotation':
    case 'scale':
      return o.transform[property]
    case 'color':
    case 'emissive':
      return hexToRgb(o.material[property] || '#000000')
    default:
      return [o.material[property]]
  }
}

export function track(o, property) {
  return (o.tracks || []).find((t) => t.property === property && t.keys.length)
}

export function valueAt(o, property, frame) {
  const t = track(o, property)
  return t ? sampleTrack(t, frame) : restValue(o, property)
}

export const isColor = (p) => COLORS.includes(p)

export const isAnimated = (o) => (o.tracks || []).length > 0

/** Transform and material at `frame` (the static ones when not animated). */
export function pose(o, frame) {
  if (!isAnimated(o)) return { transform: o.transform, material: o.material, camera: o.camera, light: o.light }
  const scalar = (p) => Math.min(RANGE[p] ?? 1, Math.max(0, valueAt(o, p, frame)[0]))
  return {
    camera: o.camera ? {...o.camera, ortho_height:o.camera.ortho_height != null ? valueAt(o,'camera_height',frame)[0] : undefined, fov:valueAt(o,'camera_fov',frame)[0], aperture:valueAt(o,'camera_aperture',frame)[0], focus:valueAt(o,'camera_focus',frame)[0]} : undefined,
    light: o.light ? {...o.light, color:rgbToHex(valueAt(o,'light_color',frame)), intensity:valueAt(o,'light_intensity',frame)[0]} : undefined,
    transform: {
      translation: valueAt(o, 'translation', frame),
      rotation: valueAt(o, 'rotation', frame),
      scale: valueAt(o, 'scale', frame),
    },
    material: {
      ...o.material,
      color: rgbToHex(valueAt(o, 'color', frame)),
      roughness: scalar('roughness'),
      metalness: scalar('metalness'),
      emissive: rgbToHex(valueAt(o, 'emissive', frame)),
      emissive_strength: scalar('emissive_strength'),
      opacity: scalar('opacity'),
    },
  }
}

/** Every keyed frame of an object, sorted. */
export function keyFrames(o) {
  const set = new Set()
  for (const t of o.tracks || []) for (const k of t.keys) set.add(k.frame)
  return [...set].sort((a, b) => a - b)
}
