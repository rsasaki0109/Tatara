// Load the exported asset with a standard glTF reader and play its bone channels.
import { AnimationMixer, Vector3 } from '../web/node_modules/three/build/three.module.js'
import { GLTFLoader } from '../web/node_modules/three/examples/jsm/loaders/GLTFLoader.js'

export async function motionTip(bytes, seconds) {
  const buffer = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength)
  const asset = await new GLTFLoader().parseAsync(buffer, '')
  const mixer = new AnimationMixer(asset.scene)
  mixer.clipAction(asset.animations[0]).play()
  mixer.setTime(seconds)
  asset.scene.updateMatrixWorld(true)
  const bone = asset.scene.getObjectByName('Bone_4') ?? asset.scene.getObjectByName('Bone 4')
  if (!bone) throw new Error('Export has no fourth joint')
  const tip = new Vector3(0, .5, 0).applyMatrix4(bone.matrixWorld).toArray()
  return { tip, channels: asset.animations[0].tracks.length }
}
