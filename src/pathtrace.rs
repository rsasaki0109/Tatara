//! Path tracer: physically based light transport over the same scene the
//! viewport and agent renders show, with soft shadows, glossy and mirror
//! reflections, refraction through glass, colour bleeding and emissive
//! surfaces that light their surroundings.
//!
//! Triangles come from the agent renderer's preparation (modifiers, poses,
//! textures and node materials) and sit in a bounding volume hierarchy.
//! One call traces a few samples per pixel; callers average calls with
//! different seeds, so the image refines progressively. The lighting is
//! the scene's world (`world.rs`): by default the viewport's warm key
//! light, cool rim light and studio dome; otherwise an environment map,
//! sampled by brightness and weighed against the surfaces' own sampling
//! (multiple importance sampling), so suns and skies both converge. The
//! floor only catches shadows and shows the grid over a transparent
//! background, so the result composites onto the viewport's backdrop,
//! unless the world is shown behind the scene.

use std::sync::{Arc, Mutex};

use glam::{DVec2, DVec3};

use crate::engine::{Editor, EngineError, Scene};
use crate::render::{Prepared, hex, prepare};
use crate::world::{EnvMap, World, env_map, turn};

/// A viewport-like camera; `fov` is the vertical field of view in degrees.
/// A lens of radius `aperture` (metres; 0 for a pinhole) blurs what is not
/// `focus` metres away (0: the distance to `target`): depth of field.
#[derive(Debug, Clone, PartialEq)]
pub struct Camera {
    pub eye: DVec3,
    pub target: DVec3,
    pub fov: f64,
    pub width: usize,
    pub height: usize,
    pub aperture: f64,
    pub focus: f64,
}

impl Camera {
    /// A pinhole camera.
    pub fn new(eye: DVec3, target: DVec3, fov: f64, width: usize, height: usize) -> Self {
        Self {
            eye,
            target,
            fov,
            width,
            height,
            aperture: 0.0,
            focus: 0.0,
        }
    }
}

const MAX_BOUNCES: u32 = 8;
const EPS: f64 = 1e-5;
const LEAF: usize = 4;
const STACK: usize = 64;
/// The viewport's backdrop (linear), seen through glass.
const BACKDROP: DVec3 = DVec3::new(0.028, 0.03, 0.035);
/// Diffuse reflectance of the floor in reflections and bounce light.
const FLOOR: f64 = 0.16;
/// How dark the floor's shadows get (the viewport's shadow opacity).
const SHADOW: f64 = 0.32;
/// Brightest a single indirect sample may be (keeps fireflies down).
const CLAMP: f64 = 4.0;

/// A directional light with a small angular size (soft shadows).
struct Light {
    dir: DVec3,
    radiance: DVec3,
    spread: f64,
}

fn studio_lights() -> [Light; 2] {
    [
        Light {
            dir: DVec3::new(4.5, 8.0, 3.5).normalize(),
            radiance: hex("#fff1e0") * 2.4,
            spread: 0.06,
        },
        Light {
            dir: DVec3::new(-5.0, 4.0, -6.0).normalize(),
            radiance: hex("#cfe0ff") * 0.9,
            spread: 0.12,
        },
    ]
}

/// Studio dome: a soft gradient with two large softboxes, so metals and
/// glass have something to mirror.
pub(crate) fn studio_dome(d: DVec3) -> DVec3 {
    let t = ((d.y + 0.25) / 1.15).clamp(0.0, 1.0);
    let t = t * t * (3.0 - 2.0 * t);
    let mut c = DVec3::new(0.10, 0.10, 0.11).lerp(DVec3::new(0.62, 0.63, 0.67), t);
    for (dir, size, strength) in [
        (DVec3::new(0.35, 1.0, 0.45), 0.93, 3.2),
        (DVec3::new(-1.0, 0.35, 0.25), 0.965, 2.2),
    ] {
        let k = d.dot(dir.normalize());
        if k > size {
            c += DVec3::splat(strength * ((k - size) / (1.0 - size) * 6.0).min(1.0));
        }
    }
    c * 0.55
}

/// The world as the tracer sees it.
pub(crate) struct Env {
    /// None: the studio lights and dome.
    map: Option<Arc<EnvMap>>,
    strength: f64,
    rotation: f64,
    background: bool,
}

impl Env {
    pub(crate) fn new(world: &World, scene: &Scene) -> Result<Self, EngineError> {
        Ok(Self {
            map: env_map(world, scene)?,
            strength: world.strength,
            rotation: world.rotation,
            background: world.background,
        })
    }

    /// The studio, as the viewport lights it.
    pub(crate) fn studio() -> Self {
        Self {
            map: None,
            strength: 1.0,
            rotation: 0.0,
            background: false,
        }
    }

    /// Directional lights: the studio's key and rim (turned and scaled
    /// with the world); none for environment maps, which light by
    /// themselves.
    fn lights(&self) -> Vec<Light> {
        if self.map.is_some() {
            return Vec::new();
        }
        studio_lights()
            .into_iter()
            .map(|l| Light {
                dir: turn(l.dir, self.rotation),
                radiance: l.radiance * self.strength,
                spread: l.spread,
            })
            .collect()
    }

    /// Light arriving from direction `d` (pointing away from the scene).
    fn radiance(&self, d: DVec3) -> DVec3 {
        let local = turn(d, -self.rotation);
        let c = match &self.map {
            Some(map) => map.radiance(local),
            None => studio_dome(local),
        };
        c * self.strength
    }

    /// A direction drawn by brightness and its density (maps only).
    fn sample(&self, rng: &mut Rng) -> Option<(DVec3, f64)> {
        let map = self.map.as_ref()?;
        let (d, pdf) = map.sample(rng.next(), rng.next(), rng.next(), rng.next());
        Some((turn(d, self.rotation), pdf))
    }

    fn pdf(&self, d: DVec3) -> f64 {
        self.map
            .as_ref()
            .map_or(0.0, |m| m.pdf(turn(d, -self.rotation)))
    }
}

/// Weight of a sample with density `a` against another strategy's `b`
/// (the power heuristic).
fn mis(a: f64, b: f64) -> f64 {
    let (a2, b2) = (a * a, b * b);
    if a2 + b2 > 0.0 { a2 / (a2 + b2) } else { 0.0 }
}

#[derive(Clone, Copy)]
struct Node {
    lo: DVec3,
    hi: DVec3,
    /// A leaf's first triangle, or an inner node's right child (its left
    /// child is the next node).
    first: u32,
    /// Triangles in a leaf; 0 for inner nodes.
    count: u32,
}

/// A triangle ready for intersection.
struct Packed {
    p0: DVec3,
    e1: DVec3,
    e2: DVec3,
    /// Index into the prepared triangles.
    tri: u32,
}

/// The scene prepared for tracing: shading data and the hierarchy.
pub struct Traced {
    prep: Prepared,
    nodes: Vec<Node>,
    packed: Vec<Packed>,
    /// Glowing triangles with their running share of the emitted power,
    /// so direct light from them can be sampled.
    emitters: Vec<(u32, f64)>,
    /// The image being refined for the latest camera.
    progress: Mutex<Option<Progressive>>,
    env: Env,
}

/// Samples gathered so far for one camera.
struct Progressive {
    camera: Camera,
    samples: u32,
    sum: Vec<[f64; 4]>,
    features: Vec<Feature>,
}

/// What a pixel looks at first: which object, its colour, facing and
/// distance. The denoiser only blends pixels that agree on these.
#[derive(Clone, Copy, Default)]
pub(crate) struct Feature {
    id: u32,
    albedo: glam::Vec3,
    normal: glam::Vec3,
    depth: f32,
    /// Mirror-like or glass: what it shows is reflected or refracted
    /// detail, not its own colour, so it is only lightly smoothed.
    sharp: bool,
}

const EMPTY_ID: u32 = u32::MAX;
const FLOOR_ID: u32 = u32::MAX - 1;

impl std::fmt::Debug for Traced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Traced")
            .field("triangles", &self.packed.len())
            .field("nodes", &self.nodes.len())
            .finish()
    }
}

/// The last prepared scene, reused while the revision and frame match.
pub type Cache = Mutex<Option<(u64, Option<u64>, Arc<Traced>)>>;

/// The scene as of `ed`'s revision (posed at `frame`), built once and
/// reused by later passes.
pub fn traced(ed: &Editor, frame: Option<f64>) -> Result<Arc<Traced>, EngineError> {
    let key = (ed.scene().revision, frame.map(f64::to_bits));
    let mut cache = ed.trace_cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((rev, f, t)) = cache.as_ref()
        && (*rev, *f) == key
    {
        return Ok(t.clone());
    }
    let env = Env::new(&ed.scene().world, ed.scene())?;
    let t = Arc::new(Traced::new(prepare(ed, None, frame)?, env));
    *cache = Some((key.0, key.1, t.clone()));
    Ok(t)
}

/// The scene posed at `frame`, prepared afresh (for one-off renders that
/// should not displace the preview's cached scene).
pub fn traced_uncached(ed: &Editor, frame: Option<f64>) -> Result<Arc<Traced>, EngineError> {
    let env = Env::new(&ed.scene().world, ed.scene())?;
    Ok(Arc::new(Traced::new(prepare(ed, None, frame)?, env)))
}

struct Hit {
    t: f64,
    packed: usize,
    u: f64,
    v: f64,
}

impl Traced {
    pub(crate) fn new(prep: Prepared, env: Env) -> Self {
        let n = prep.tris.len();
        let bounds: Vec<(DVec3, DVec3)> = prep
            .tris
            .iter()
            .map(|t| {
                (
                    t.p[0].min(t.p[1]).min(t.p[2]),
                    t.p[0].max(t.p[1]).max(t.p[2]),
                )
            })
            .collect();
        let centres: Vec<DVec3> = bounds.iter().map(|(a, b)| (*a + *b) * 0.5).collect();
        let mut order: Vec<u32> = (0..n as u32).collect();
        let mut nodes = Vec::with_capacity(2 * n / LEAF + 1);
        if n > 0 {
            build(&mut nodes, &mut order, 0, &bounds, &centres, 0);
        }
        let packed = order
            .iter()
            .map(|&i| {
                let p = prep.tris[i as usize].p;
                Packed {
                    p0: p[0],
                    e1: p[1] - p[0],
                    e2: p[2] - p[0],
                    tri: i,
                }
            })
            .collect();
        let mut total = 0.0;
        let mut emitters: Vec<(u32, f64)> = Vec::new();
        for (i, t) in prep.tris.iter().enumerate() {
            let e = prep.materials[t.object as usize].emissive;
            let area = (t.p[1] - t.p[0]).cross(t.p[2] - t.p[0]).length() * 0.5;
            if e.max_element() > 0.0 && area > 1e-12 {
                total += area * lum(e);
                emitters.push((i as u32, total));
            }
        }
        for e in &mut emitters {
            e.1 /= total;
        }
        Self {
            emitters,
            progress: Mutex::new(None),
            prep,
            nodes,
            packed,
            env,
        }
    }

    /// Visit every leaf whose box the ray enters before `tmax`.
    fn walk(
        &self,
        o: DVec3,
        d: DVec3,
        tmax: &mut f64,
        mut leaf: impl FnMut(&[Packed], usize, &mut f64) -> bool,
    ) {
        if self.nodes.is_empty() {
            return;
        }
        let inv = d.recip();
        let mut stack = [0usize; STACK];
        let mut sp = 1;
        while sp > 0 {
            sp -= 1;
            let index = stack[sp];
            let node = &self.nodes[index];
            if !slab(node.lo, node.hi, o, inv, *tmax) {
                continue;
            }
            if node.count > 0 {
                let first = node.first as usize;
                let tris = &self.packed[first..first + node.count as usize];
                if leaf(tris, first, tmax) {
                    return;
                }
            } else if sp + 2 <= STACK {
                stack[sp] = node.first as usize;
                stack[sp + 1] = index + 1;
                sp += 2;
            }
        }
    }

    fn hit(&self, o: DVec3, d: DVec3, tmax: f64) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        let mut limit = tmax;
        self.walk(o, d, &mut limit, |tris, first, limit| {
            for (k, p) in tris.iter().enumerate() {
                if let Some((t, u, v)) = intersect(p, o, d)
                    && t < *limit
                {
                    *limit = t;
                    best = Some(Hit {
                        t,
                        packed: first + k,
                        u,
                        v,
                    });
                }
            }
            false
        });
        best
    }

    /// Light passing from `o` along `d` up to `tmax`: glass tints it,
    /// translucent surfaces thin it and anything opaque blocks it.
    fn transmittance(&self, o: DVec3, d: DVec3, tmax: f64) -> DVec3 {
        let mut through = DVec3::ONE;
        let mut limit = tmax;
        // A ray through a shared edge or vertex hits every triangle there;
        // count each crossing once.
        let mut seen = [f64::NAN; 16];
        let mut n = 0;
        self.walk(o, d, &mut limit, |tris, _, _| {
            for p in tris {
                let Some((t, _, _)) = intersect(p, o, d).filter(|(t, ..)| *t < tmax) else {
                    continue;
                };
                if seen.iter().any(|s| (s - t).abs() <= 1e-7 * t.max(1.0)) {
                    continue;
                }
                seen[n % seen.len()] = t;
                n += 1;
                through *= self.prep.materials[self.prep.tris[p.tri as usize].object as usize]
                    .shadow_tint();
                if through.max_element() < 1e-3 {
                    through = DVec3::ZERO;
                    return true;
                }
            }
            false
        });
        through
    }

    /// Trace `samples` paths through each pixel and return their mean as
    /// premultiplied linear RGBA, row by row from the top. Alpha is how
    /// much of the pixel is covered (the empty backdrop is transparent).
    pub fn render(&self, camera: &Camera, samples: u32, seed: u32) -> Vec<[f32; 4]> {
        let (w, h) = (camera.width, camera.height);
        let frame = Frame::new(camera);
        let lights = self.env.lights();
        rows(w, h, |x, y| {
            let mut acc = [0f64; 4];
            for s in 0..samples {
                let mut rng = Rng::new(hash3(
                    x as u32,
                    y as u32,
                    seed.wrapping_mul(977).wrapping_add(s),
                ));
                let jx = (x as f64 + rng.next()) / w as f64;
                let jy = (y as f64 + rng.next()) / h as f64;
                let (o, d) = frame.shoot(jx, jy, &mut rng);
                let (c, a) = self.path(o, d, &lights, &mut rng);
                acc[0] += c.x;
                acc[1] += c.y;
                acc[2] += c.z;
                acc[3] += a;
            }
            acc.map(|v| (v / samples as f64) as f32)
        })
    }

    /// A camera position framing the whole scene from a named view: the
    /// eye and the point it looks at.
    pub fn frame_view(&self, view: &crate::render::View, fov: f64, aspect: f64) -> (DVec3, DVec3) {
        let half = (fov.to_radians() / 2.0).tan() * aspect.min(1.0);
        let distance = self.prep.radius * 1.12 / half.atan().sin();
        let (az, el) = (
            view.azimuth.to_radians(),
            view.elevation.clamp(-89.5, 89.5).to_radians(),
        );
        let dir = DVec3::new(el.cos() * az.sin(), el.sin(), el.cos() * az.cos());
        (self.prep.center + dir * distance, self.prep.center)
    }

    /// A finished still: `samples` per pixel, denoised and tone-mapped,
    /// over the viewport's studio backdrop or transparent (straight sRGB
    /// RGBA bytes).
    pub fn still(&self, camera: &Camera, samples: u32, transparent: bool) -> Vec<u8> {
        let image = self.render(camera, samples, 0);
        let features = self.features(camera);
        let mut out = bytes(&self.develop(&image, &features, camera, samples));
        if !transparent {
            backdrop(&mut out, camera.width, camera.height);
        }
        out
    }

    /// What the camera sees first through each pixel's centre, to guide
    /// the denoiser.
    pub(crate) fn features(&self, camera: &Camera) -> Vec<Feature> {
        let (w, h) = (camera.width, camera.height);
        let frame = Frame::new(camera);
        let floor_visible = camera.eye.y > 0.0;
        rows(w, h, |x, y| {
            let d = frame.ray((x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64);
            let o = camera.eye;
            let ground = (floor_visible && d.y < -1e-9)
                .then(|| -o.y / d.y)
                .filter(|t| *t > EPS);
            match self.hit(o, d, ground.unwrap_or(f64::INFINITY)) {
                Some(hit) => {
                    let packed = &self.packed[hit.packed];
                    let tri = &self.prep.tris[packed.tri as usize];
                    let m = &self.prep.materials[tri.object as usize];
                    let b = [1.0 - hit.u - hit.v, hit.u, hit.v];
                    let geo = packed.e1.cross(packed.e2).normalize_or(DVec3::Y);
                    let n = (tri.n[0] * b[0] + tri.n[1] * b[1] + tri.n[2] * b[2]).normalize_or(geo);
                    let uv = tri.uv[0] * b[0] + tri.uv[1] * b[1] + tri.uv[2] * b[2];
                    let pt = m.at(o + d * hit.t, n, tri.tangents, uv);
                    let n = if pt.normal.dot(d) > 0.0 {
                        -pt.normal
                    } else {
                        pt.normal
                    };
                    Feature {
                        id: tri.object,
                        albedo: pt.albedo.as_vec3(),
                        normal: n.as_vec3(),
                        depth: hit.t as f32,
                        sharp: m.transmission > 0.3 || (pt.metalness > 0.5 && pt.roughness < 0.35),
                    }
                }
                None => Feature {
                    id: if ground.is_some() { FLOOR_ID } else { EMPTY_ID },
                    albedo: glam::Vec3::ONE,
                    normal: glam::Vec3::Y,
                    depth: ground.unwrap_or(0.0) as f32,
                    sharp: false,
                },
            }
        })
    }

    /// Add `samples` per pixel to the progressive image for `camera`
    /// (starting over when the camera changed) and return how many samples
    /// it now holds and its denoised, tone-mapped pixels as straight sRGB
    /// RGBA bytes.
    pub fn pass(&self, camera: &Camera, samples: u32) -> (u32, Vec<u8>) {
        let start = {
            let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
            match p.as_ref() {
                Some(p) if p.camera == *camera => p.samples,
                _ => {
                    *p = None;
                    0
                }
            }
        };
        let features = (start == 0).then(|| self.features(camera));
        let image = self.render(camera, samples, start);
        let mut guard = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(features) = features
            && guard.as_ref().is_none_or(|p| p.camera != *camera)
        {
            *guard = Some(Progressive {
                camera: camera.clone(),
                samples: 0,
                sum: vec![[0.0; 4]; image.len()],
                features,
            });
        }
        let Some(p) = guard.as_mut().filter(|p| p.camera == *camera) else {
            // Another camera took over meanwhile; show this pass alone.
            let f = self.features(camera);
            return (samples, bytes(&self.develop(&image, &f, camera, samples)));
        };
        for (acc, px) in p.sum.iter_mut().zip(&image) {
            for k in 0..4 {
                acc[k] += px[k] as f64 * samples as f64;
            }
        }
        p.samples += samples;
        let n = p.samples as f64;
        let mean: Vec<[f32; 4]> = p.sum.iter().map(|a| a.map(|v| (v / n) as f32)).collect();
        let image = self.develop(&mean, &p.features, camera, p.samples);
        (p.samples, bytes(&image))
    }

    /// One path: (premultiplied radiance, coverage).
    fn path(&self, eye: DVec3, dir: DVec3, lights: &[Light], rng: &mut Rng) -> (DVec3, f64) {
        let mut o = eye;
        let mut d = dir;
        let mut throughput = DVec3::ONE;
        let mut color = DVec3::ZERO;
        // Still looking straight through (only refraction or see-through
        // surfaces so far): misses show the backdrop, the floor its shadow.
        let mut film = true;
        // Whether hitting a glowing surface counts: not after diffuse or
        // glossy bounces, whose light from it was already sampled directly.
        let mut emission = true;
        // Density of the last bounce's direction, to weigh the world's
        // light it finds against sampling the world directly.
        let mut last_pdf = 0.0;
        let mut bounce = 0;
        let mut steps = 0;
        let floor_visible = eye.y > 0.0;
        while steps < 64 {
            steps += 1;
            let ground = (floor_visible && d.y < -1e-9)
                .then(|| -o.y / d.y)
                .filter(|t| *t > EPS);
            let hit = self.hit(o, d, ground.unwrap_or(f64::INFINITY));
            let Some(hit) = hit else {
                if let Some(t) = ground {
                    let p = o + d * t;
                    if film {
                        if bounce == 0 && steps == 1 {
                            let (c, shadow) = self.floor(p, lights, rng);
                            if self.env.background {
                                return (self.env.radiance(d) * (1.0 - shadow), 1.0);
                            }
                            return (c, shadow);
                        }
                        color += throughput * self.backdrop(d);
                        return (color, 1.0);
                    }
                    // A dim matte floor in reflections and bounce light.
                    let n = DVec3::Y;
                    let albedo = DVec3::splat(FLOOR);
                    color += clamp(
                        throughput
                            * self.direct(
                                p,
                                n,
                                -d,
                                |_v, l, _| albedo / std::f64::consts::PI * n.dot(l).max(0.0),
                                |l| n.dot(l).max(0.0) / std::f64::consts::PI,
                                lights,
                                rng,
                            ),
                        bounce,
                    );
                    emission = false;
                    throughput *= albedo;
                    o = p + n * EPS;
                    d = cosine(n, rng);
                    last_pdf = n.dot(d).max(0.0) / std::f64::consts::PI;
                    bounce += 1;
                    if bounce > MAX_BOUNCES || !survive(&mut throughput, bounce, rng) {
                        break;
                    }
                    continue;
                }
                if film {
                    if steps == 1 && !self.env.background {
                        return (DVec3::ZERO, 0.0);
                    }
                    color += throughput * self.backdrop(d);
                } else {
                    // Light the world gave by direct sampling is counted
                    // there, in proportion (mirrors and glass see it here).
                    let weight = if emission {
                        1.0
                    } else if self.env.map.is_some() {
                        mis(last_pdf, self.env.pdf(d))
                    } else {
                        1.0
                    };
                    color += clamp(throughput * self.env.radiance(d) * weight, bounce);
                }
                return (color, 1.0);
            };
            let packed = &self.packed[hit.packed];
            let tri = &self.prep.tris[packed.tri as usize];
            let m = &self.prep.materials[tri.object as usize];
            let p = o + d * hit.t;
            let b = [1.0 - hit.u - hit.v, hit.u, hit.v];
            let geo = packed.e1.cross(packed.e2).normalize_or(DVec3::Y);
            let entering = d.dot(geo) < 0.0;
            let shading = (tri.n[0] * b[0] + tri.n[1] * b[1] + tri.n[2] * b[2]).normalize_or(geo);
            let uv: DVec2 = tri.uv[0] * b[0] + tri.uv[1] * b[1] + tri.uv[2] * b[2];
            let pt = m.at(p, shading, tri.tangents, uv);
            if emission {
                color += clamp(throughput * m.emissive, bounce);
            }

            // See-through: the path continues as if nothing were there.
            if rng.next() >= m.opacity {
                o = p + d * EPS * 4.0;
                continue;
            }
            // Face the side the ray came from.
            let side = if entering { 1.0 } else { -1.0 };
            let ng = geo * side;
            let mut n = pt.normal * side;
            if n.dot(ng) <= 0.0 {
                n = ng;
            }
            let v = -d;

            if m.transmission > 0.0 && rng.next() < m.transmission {
                // Glass: Fresnel-weighted reflection or refraction through a
                // (rough) microfacet; entering light takes the glass colour.
                let alpha = (pt.roughness * pt.roughness).max(1e-4);
                let h = if alpha > 1e-3 {
                    to_world(
                        n,
                        ggx_visible(to_local(n, v), alpha, rng.next(), rng.next()),
                    )
                } else {
                    n
                };
                let eta = if entering { 1.0 / 1.5 } else { 1.5 };
                let cos_i = v.dot(h).max(0.0);
                let f = fresnel(cos_i, eta);
                emission = true;
                if rng.next() < f {
                    d = reflect(-v, h);
                    film = false;
                    o = p + ng * EPS;
                } else if let Some(t) = refract(-v, h, eta) {
                    d = t;
                    if entering {
                        throughput *= DVec3::ONE.lerp(pt.albedo, 0.8);
                    }
                    o = p - ng * EPS;
                } else {
                    d = reflect(-v, h);
                    o = p + ng * EPS;
                }
                bounce += 1;
                if bounce > MAX_BOUNCES || !survive(&mut throughput, bounce, rng) {
                    break;
                }
                continue;
            }

            film = false;
            let alpha = (pt.roughness * pt.roughness).max(2e-3);
            // Near-mirrors see glowing surfaces by hitting them, not by
            // sampling them (their highlight would be too narrow to find).
            let mirror = alpha < 0.02;
            let f0 = DVec3::splat(0.04).lerp(pt.albedo, pt.metalness);
            let nv = n.dot(v).max(1e-4);
            let diffuse = pt.albedo * (1.0 - pt.metalness);
            let fv = schlick(f0, nv);
            let p_spec = if diffuse.max_element() < 1e-4 {
                1.0
            } else {
                (lum(fv) + 0.5 * pt.metalness).clamp(0.1, 0.9)
            };
            color += clamp(
                throughput
                    * self.direct(
                        p + ng * EPS,
                        n,
                        v,
                        |v, l, area| {
                            let spec = if area && mirror { DVec3::ZERO } else { f0 };
                            principled(n, v, l, diffuse, spec, alpha.max(0.02))
                        },
                        |l| bsdf_pdf(n, v, l, alpha, p_spec, mirror),
                        lights,
                        rng,
                    ),
                bounce,
            );
            if rng.next() < p_spec {
                let h = to_world(
                    n,
                    ggx_visible(to_local(n, v), alpha, rng.next(), rng.next()),
                );
                let l = reflect(-v, h);
                let nl = n.dot(l);
                if nl <= 0.0 || l.dot(ng) <= 0.0 {
                    break;
                }
                throughput *= schlick(f0, v.dot(h).max(0.0)) * smith(alpha, nl) / p_spec;
                d = l;
                emission = mirror;
            } else {
                d = cosine(n, rng);
                if d.dot(ng) <= 0.0 {
                    break;
                }
                throughput *= diffuse * (DVec3::ONE - fv) / (1.0 - p_spec);
                emission = false;
            }
            last_pdf = bsdf_pdf(n, v, d, alpha, p_spec, mirror);
            o = p + ng * EPS;
            bounce += 1;
            if bounce > MAX_BOUNCES || !survive(&mut throughput, bounce, rng) {
                break;
            }
        }
        (color, 1.0)
    }

    /// Light reaching `p` straight from the key and rim lights, from one
    /// direction of the world's environment map and from one sampled
    /// glowing triangle, shaped by `bsdf(v, l, from_area)` (which includes
    /// the cosine). `pdf(l)` is how likely the surface's own sampling is to
    /// pick `l`, to share the world's light with it.
    fn direct(
        &self,
        p: DVec3,
        n: DVec3,
        v: DVec3,
        bsdf: impl Fn(DVec3, DVec3, bool) -> DVec3,
        pdf: impl Fn(DVec3) -> f64,
        lights: &[Light],
        rng: &mut Rng,
    ) -> DVec3 {
        let mut sum = DVec3::ZERO;
        if let Some((l, density)) = self.env.sample(rng)
            // The floor hides the world below the horizon.
            && !(l.y < 0.0 && p.y > 0.0)
            && n.dot(l) > 0.0
            && density > 0.0
        {
            let through = self.transmittance(p, l, f64::INFINITY);
            if through != DVec3::ZERO {
                let weight = mis(density, pdf(l));
                sum += bsdf(v, l, true) * self.env.radiance(l) * through * (weight / density);
            }
        }
        for light in lights {
            let l = cone(light.dir, light.spread, rng);
            if n.dot(l) <= 0.0 {
                continue;
            }
            let through = self.transmittance(p, l, f64::INFINITY);
            if through != DVec3::ZERO {
                sum += bsdf(v, l, false) * light.radiance * through;
            }
        }
        if self.emitters.is_empty() {
            return sum;
        }
        // Pick a glowing triangle by its power, then a point on it.
        let r = rng.next();
        let k = self
            .emitters
            .partition_point(|e| e.1 < r)
            .min(self.emitters.len() - 1);
        let (index, cdf) = self.emitters[k];
        let share = cdf - if k > 0 { self.emitters[k - 1].1 } else { 0.0 };
        let tri = &self.prep.tris[index as usize];
        let (e1, e2) = (tri.p[1] - tri.p[0], tri.p[2] - tri.p[0]);
        let cross = e1.cross(e2);
        let area = cross.length() * 0.5;
        let (r1, r2) = (rng.next().sqrt(), rng.next());
        let q = tri.p[0] + e1 * (r1 * (1.0 - r2)) + e2 * (r1 * r2);
        let to = q - p;
        let dist2 = to.length_squared();
        let dist = dist2.sqrt();
        let l = to / dist;
        let cos_light = (cross / (2.0 * area)).dot(l).abs();
        if n.dot(l) <= 0.0 || cos_light < 1e-6 || share <= 0.0 {
            return sum;
        }
        let through = self.transmittance(p, l, dist * (1.0 - 1e-4));
        if through == DVec3::ZERO {
            return sum;
        }
        // Area density turned into a density over directions.
        let pdf = share / area * dist2 / cos_light;
        let glow = self.prep.materials[tri.object as usize].emissive;
        sum + bsdf(v, l, true) * glow * through / pdf
    }

    /// The floor seen directly: its grid plus the shadows on it, over the
    /// transparent backdrop.
    fn floor(&self, p: DVec3, lights: &[Light], rng: &mut Rng) -> (DVec3, f64) {
        // The main light: the key light, or a bright direction of the
        // world (mostly its sun, if it has one).
        let l = match (lights.first(), self.env.sample(rng)) {
            (Some(key), _) => cone(key.dir, key.spread, rng),
            (None, Some((l, _))) if l.y > 0.0 => l,
            _ => DVec3::Y,
        };
        let sun = self.transmittance(p + DVec3::Y * EPS, l, f64::INFINITY);
        let sun = sun.dot(DVec3::splat(1.0 / 3.0));
        // Contact shadow: is the sky above this point blocked nearby?
        let up = cosine(DVec3::Y, rng);
        let sky = if self
            .hit(p + DVec3::Y * EPS, up, self.radius() * 2.0)
            .is_some()
        {
            0.0
        } else {
            1.0
        };
        // The grid goes on after denoising (`develop`), which would blur it.
        (DVec3::ZERO, SHADOW * (1.0 - (0.8 * sun + 0.2 * sky)))
    }

    /// Finish a mean image: denoise it, draw the floor grid under the
    /// shadows and let bright light glow (premultiplied linear RGBA).
    pub(crate) fn develop(
        &self,
        mean: &[[f32; 4]],
        features: &[Feature],
        camera: &Camera,
        samples: u32,
    ) -> Vec<[f32; 4]> {
        let (w, h) = (camera.width, camera.height);
        let clean = denoise(mean, features, w, h, samples);
        let frame = Frame::new(camera);
        let gridded = rows(w, h, |x, y| {
            let i = y * w + x;
            let c = clean[i];
            if features[i].id != FLOOR_ID {
                return c;
            }
            // Coverage from four rays across the pixel.
            let (mut colour, mut line) = (DVec3::ZERO, 0.0);
            for (sx, sy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let d = frame.ray((x as f64 + sx) / w as f64, (y as f64 + sy) / h as f64);
                if d.y >= -1e-9 {
                    continue;
                }
                let t = -camera.eye.y / d.y;
                let (g, a) = grid(camera.eye + d * t, t * frame.pixel);
                colour += g * a * 0.25;
                line += a * 0.25;
            }
            // Grid lines under the shadow, both over the clear backdrop.
            let shade = c[3];
            let under = colour * (1.0 - shade as f64);
            [
                c[0] + under.x as f32,
                c[1] + under.y as f32,
                c[2] + under.z as f32,
                shade + line as f32 * (1.0 - shade),
            ]
        });
        bloom(&gridded, w, h)
    }

    fn radius(&self) -> f64 {
        self.prep.radius.max(0.5)
    }

    /// What shows straight through glass and see-through surfaces: the
    /// world when it is the background, else the viewport's backdrop.
    fn backdrop(&self, d: DVec3) -> DVec3 {
        if self.env.background {
            self.env.radiance(d)
        } else {
            BACKDROP
        }
    }
}

/// How likely a surface's sampling (GGX visible normals with probability
/// `p_spec`, else cosine) is to pick `l`, over solid angle. Near-mirror
/// reflection is left out: it finds lights by hitting them.
fn bsdf_pdf(n: DVec3, v: DVec3, l: DVec3, alpha: f64, p_spec: f64, mirror: bool) -> f64 {
    let nl = n.dot(l);
    if nl <= 0.0 {
        return 0.0;
    }
    let diffuse = (1.0 - p_spec) * nl / std::f64::consts::PI;
    if mirror {
        return diffuse;
    }
    let nv = n.dot(v).max(1e-4);
    let h = (v + l).normalize();
    let nh = n.dot(h).max(0.0);
    let a2 = alpha * alpha;
    let den = nh * nh * (a2 - 1.0) + 1.0;
    let dist = a2 / (std::f64::consts::PI * den * den);
    diffuse + p_spec * dist * smith(alpha, nv) / (4.0 * nv)
}

/// Grid lines every half metre within 8 m (the viewport's grid): colour
/// and coverage at floor point `p` for a pixel `width` wide there.
fn grid(p: DVec3, width: f64) -> (DVec3, f64) {
    if p.x.abs() > 8.0 || p.z.abs() > 8.0 {
        return (DVec3::ZERO, 0.0);
    }
    let w = width.max(1e-6);
    let line = |x: f64| {
        let f = (x / 0.5 - (x / 0.5).round()).abs() * 0.5;
        (1.0 - f / w).clamp(0.0, 1.0)
    };
    let centre = (1.0 - p.x.abs() / w)
        .clamp(0.0, 1.0)
        .max((1.0 - p.z.abs() / w).clamp(0.0, 1.0));
    let any = line(p.x).max(line(p.z));
    let colour = hex("#5a5f69").lerp(hex("#8a8f99"), centre);
    (colour, any * 0.16)
}

fn lum(c: DVec3) -> f64 {
    c.dot(DVec3::new(0.2126, 0.7152, 0.0722))
}

/// Cap one indirect sample's brightness.
fn clamp(c: DVec3, bounce: u32) -> DVec3 {
    let m = c.max_element();
    if bounce > 0 && m > CLAMP {
        c * (CLAMP / m)
    } else {
        c
    }
}

/// Russian roulette after a few bounces; false ends the path.
fn survive(throughput: &mut DVec3, bounce: u32, rng: &mut Rng) -> bool {
    if bounce < 3 {
        return true;
    }
    let p = throughput.max_element().clamp(0.05, 0.95);
    if rng.next() > p {
        return false;
    }
    *throughput /= p;
    true
}

/// Cook-Torrance GGX specular plus Lambert diffuse, times the cosine.
fn principled(n: DVec3, v: DVec3, l: DVec3, diffuse: DVec3, f0: DVec3, alpha: f64) -> DVec3 {
    let (nl, nv) = (n.dot(l), n.dot(v));
    if nl <= 0.0 || nv <= 0.0 {
        return DVec3::ZERO;
    }
    let h = (v + l).normalize();
    let nh = n.dot(h).max(0.0);
    let f = schlick(f0, v.dot(h).max(0.0));
    let a2 = alpha * alpha;
    let den = nh * nh * (a2 - 1.0) + 1.0;
    let dist = a2 / (std::f64::consts::PI * den * den);
    let spec = f * (dist * smith(alpha, nl) * smith(alpha, nv) / (4.0 * nv * nl));
    (diffuse * (DVec3::ONE - schlick(f0, nv)) / std::f64::consts::PI + spec) * nl
}

/// Smith's masking for GGX.
fn smith(alpha: f64, cos: f64) -> f64 {
    let c2 = (cos * cos).max(1e-12);
    2.0 / (1.0 + (1.0 + alpha * alpha * (1.0 - c2) / c2).sqrt())
}

fn schlick(f0: DVec3, cos: f64) -> DVec3 {
    f0 + (DVec3::ONE - f0) * (1.0 - cos).clamp(0.0, 1.0).powi(5)
}

/// Unpolarized Fresnel reflectance; `eta` = incident / transmitted index.
fn fresnel(cos_i: f64, eta: f64) -> f64 {
    let sin2 = eta * eta * (1.0 - cos_i * cos_i);
    if sin2 >= 1.0 {
        return 1.0;
    }
    let cos_t = (1.0 - sin2).sqrt();
    let rs = (eta * cos_i - cos_t) / (eta * cos_i + cos_t);
    let rp = (cos_i - eta * cos_t) / (cos_i + eta * cos_t);
    0.5 * (rs * rs + rp * rp)
}

fn reflect(d: DVec3, n: DVec3) -> DVec3 {
    d - n * 2.0 * d.dot(n)
}

fn refract(d: DVec3, n: DVec3, eta: f64) -> Option<DVec3> {
    let cos_i = -d.dot(n);
    let k = 1.0 - eta * eta * (1.0 - cos_i * cos_i);
    (k >= 0.0).then(|| (d * eta + n * (eta * cos_i - k.sqrt())).normalize())
}

/// An orthonormal basis around `n` (Duff et al.).
fn basis(n: DVec3) -> (DVec3, DVec3) {
    let s = if n.z >= 0.0 { 1.0 } else { -1.0 };
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    (
        DVec3::new(1.0 + s * n.x * n.x * a, s * b, -s * n.x),
        DVec3::new(b, s + n.y * n.y * a, -n.y),
    )
}

fn to_local(n: DVec3, v: DVec3) -> DVec3 {
    let (t, b) = basis(n);
    DVec3::new(v.dot(t), v.dot(b), v.dot(n))
}

fn to_world(n: DVec3, v: DVec3) -> DVec3 {
    let (t, b) = basis(n);
    (t * v.x + b * v.y + n * v.z).normalize()
}

fn cosine(n: DVec3, rng: &mut Rng) -> DVec3 {
    let (u1, u2) = (rng.next(), rng.next());
    let r = u1.sqrt();
    let phi = std::f64::consts::TAU * u2;
    to_world(
        n,
        DVec3::new(r * phi.cos(), r * phi.sin(), (1.0 - u1).max(0.0).sqrt()),
    )
}

/// A direction within `spread` radians of `dir`.
fn cone(dir: DVec3, spread: f64, rng: &mut Rng) -> DVec3 {
    let cos_max = spread.cos();
    let z = 1.0 - rng.next() * (1.0 - cos_max);
    let r = (1.0 - z * z).max(0.0).sqrt();
    let phi = std::f64::consts::TAU * rng.next();
    to_world(dir, DVec3::new(r * phi.cos(), r * phi.sin(), z))
}

/// A GGX microfacet normal seen from local direction `v` (Heitz 2018).
fn ggx_visible(v: DVec3, alpha: f64, u1: f64, u2: f64) -> DVec3 {
    let vh = DVec3::new(alpha * v.x, alpha * v.y, v.z.max(1e-6)).normalize();
    let len2 = vh.x * vh.x + vh.y * vh.y;
    let t1 = if len2 > 0.0 {
        DVec3::new(-vh.y, vh.x, 0.0) / len2.sqrt()
    } else {
        DVec3::X
    };
    let t2 = vh.cross(t1);
    let r = u1.sqrt();
    let phi = std::f64::consts::TAU * u2;
    let p1 = r * phi.cos();
    let s = 0.5 * (1.0 + vh.z);
    let p2 = (1.0 - s) * (1.0 - p1 * p1).max(0.0).sqrt() + s * r * phi.sin();
    let nh = t1 * p1 + t2 * p2 + vh * (1.0 - p1 * p1 - p2 * p2).max(0.0).sqrt();
    DVec3::new(alpha * nh.x, alpha * nh.y, nh.z.max(1e-6)).normalize()
}

/// The camera's ray directions, and how wide a pixel is at unit distance.
struct Frame {
    eye: DVec3,
    aperture: f64,
    focus: f64,
    forward: DVec3,
    right: DVec3,
    up: DVec3,
    half_h: f64,
    half_w: f64,
    pixel: f64,
}

impl Frame {
    fn new(c: &Camera) -> Self {
        let forward = (c.target - c.eye).normalize_or(DVec3::NEG_Z);
        let right = forward.cross(DVec3::Y).normalize_or(DVec3::X);
        let up = right.cross(forward);
        let half_h = (c.fov.to_radians() / 2.0).tan();
        let aspect = c.width as f64 / c.height.max(1) as f64;
        let focus = if c.focus > 0.0 {
            c.focus
        } else {
            c.eye.distance(c.target)
        };
        Self {
            eye: c.eye,
            aperture: c.aperture.max(0.0),
            focus,
            forward,
            right,
            up,
            half_h,
            half_w: half_h * aspect,
            pixel: 2.0 * half_h / c.height.max(1) as f64,
        }
    }

    /// A ray through (x, y) in 0..1 from the top left: from the eye, or
    /// from a point on the lens towards where that ray is in focus.
    fn shoot(&self, x: f64, y: f64, rng: &mut Rng) -> (DVec3, DVec3) {
        let d = self.ray(x, y);
        if self.aperture <= 0.0 {
            return (self.eye, d);
        }
        let focal = self.eye + d * (self.focus / d.dot(self.forward).max(1e-6));
        let r = self.aperture * rng.next().sqrt();
        let phi = std::f64::consts::TAU * rng.next();
        let o = self.eye + self.right * (r * phi.cos()) + self.up * (r * phi.sin());
        (o, (focal - o).normalize())
    }

    /// The ray through (x, y) in 0..1 from the top left.
    fn ray(&self, x: f64, y: f64) -> DVec3 {
        (self.forward
            + self.right * ((2.0 * x - 1.0) * self.half_w)
            + self.up * ((1.0 - 2.0 * y) * self.half_h))
            .normalize()
    }
}

fn intersect(p: &Packed, o: DVec3, d: DVec3) -> Option<(f64, f64, f64)> {
    let pv = d.cross(p.e2);
    let det = p.e1.dot(pv);
    if det.abs() < 1e-14 {
        return None;
    }
    let inv = 1.0 / det;
    let tv = o - p.p0;
    let u = tv.dot(pv) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let qv = tv.cross(p.e1);
    let v = d.dot(qv) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = p.e2.dot(qv) * inv;
    (t > EPS).then_some((t, u, v))
}

fn slab(lo: DVec3, hi: DVec3, o: DVec3, inv: DVec3, tmax: f64) -> bool {
    let a = (lo - o) * inv;
    let b = (hi - o) * inv;
    let near = a.min(b).max_element().max(0.0);
    let far = a.max(b).min_element().min(tmax);
    near <= far
}

/// Build a subtree over `order` (triangle indices), binned SAH.
fn build(
    nodes: &mut Vec<Node>,
    order: &mut [u32],
    offset: usize,
    bounds: &[(DVec3, DVec3)],
    centres: &[DVec3],
    depth: usize,
) -> usize {
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    let (mut clo, mut chi) = (lo, hi);
    for &i in order.iter() {
        let (a, b) = bounds[i as usize];
        lo = lo.min(a);
        hi = hi.max(b);
        clo = clo.min(centres[i as usize]);
        chi = chi.max(centres[i as usize]);
    }
    let index = nodes.len();
    nodes.push(Node {
        lo,
        hi,
        first: offset as u32,
        count: order.len() as u32,
    });
    let extent = chi - clo;
    let axis = if extent.x >= extent.y && extent.x >= extent.z {
        0
    } else if extent.y >= extent.z {
        1
    } else {
        2
    };
    if order.len() <= LEAF || depth >= STACK - 4 || extent[axis] < 1e-12 {
        return index;
    }
    const BINS: usize = 12;
    let bin = |i: u32| {
        let c = (centres[i as usize][axis] - clo[axis]) / extent[axis];
        ((c * BINS as f64) as usize).min(BINS - 1)
    };
    let mut boxes = [(
        DVec3::splat(f64::INFINITY),
        DVec3::splat(f64::NEG_INFINITY),
        0usize,
    ); BINS];
    for &i in order.iter() {
        let b = &mut boxes[bin(i)];
        b.0 = b.0.min(bounds[i as usize].0);
        b.1 = b.1.max(bounds[i as usize].1);
        b.2 += 1;
    }
    let area = |a: DVec3, b: DVec3| {
        let e = (b - a).max(DVec3::ZERO);
        e.x * e.y + e.y * e.z + e.z * e.x
    };
    let mut best = (f64::INFINITY, BINS / 2);
    for split in 1..BINS {
        let side = |r: std::ops::Range<usize>| {
            r.fold(
                (
                    DVec3::splat(f64::INFINITY),
                    DVec3::splat(f64::NEG_INFINITY),
                    0,
                ),
                |acc, k| {
                    (
                        acc.0.min(boxes[k].0),
                        acc.1.max(boxes[k].1),
                        acc.2 + boxes[k].2,
                    )
                },
            )
        };
        let (l, r) = (side(0..split), side(split..BINS));
        if l.2 == 0 || r.2 == 0 {
            continue;
        }
        let cost = area(l.0, l.1) * l.2 as f64 + area(r.0, r.1) * r.2 as f64;
        if cost < best.0 {
            best = (cost, split);
        }
    }
    let mid = if best.0.is_finite() {
        let mut m = 0;
        for k in 0..order.len() {
            if bin(order[k]) < best.1 {
                order.swap(k, m);
                m += 1;
            }
        }
        m
    } else {
        0
    };
    let mid = if mid == 0 || mid == order.len() {
        let half = order.len() / 2;
        order.select_nth_unstable_by(half, |a, b| {
            centres[*a as usize][axis].total_cmp(&centres[*b as usize][axis])
        });
        half
    } else {
        mid
    };
    let (left, right) = order.split_at_mut(mid);
    build(nodes, left, offset, bounds, centres, depth + 1);
    let r = build(nodes, right, offset + mid, bounds, centres, depth + 1);
    nodes[index].first = r as u32;
    nodes[index].count = 0;
    index
}

/// Evaluate `pixel(x, y)` over a `w` x `h` image, rows split across
/// threads where there are any.
fn rows<T: Send + Copy + Default>(
    w: usize,
    h: usize,
    pixel: impl Fn(usize, usize) -> T + Sync,
) -> Vec<T> {
    let mut out = vec![T::default(); w * h];
    let fill = |chunk: &mut [T], y0: usize| {
        for (k, px) in chunk.iter_mut().enumerate() {
            *px = pixel(k % w, y0 + k / w);
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(16);
        let per = h.div_ceil(threads).max(1) * w;
        let fill = &fill;
        std::thread::scope(|scope| {
            for (i, chunk) in out.chunks_mut(per.max(1)).enumerate() {
                scope.spawn(move || fill(chunk, i * per / w.max(1)));
            }
        });
    }
    #[cfg(target_arch = "wasm32")]
    fill(&mut out, 0);
    out
}

/// Smooth away sampling noise while keeping edges: an edge-avoiding
/// À-trous wavelet filter (Dammertz et al.) that only blends pixels which
/// see the same object with similar colour, facing and depth. It works on
/// premultiplied linear RGBA and returns the same.
pub(crate) fn denoise(
    image: &[[f32; 4]],
    features: &[Feature],
    w: usize,
    h: usize,
    samples: u32,
) -> Vec<[f32; 4]> {
    const KERNEL: [f32; 5] = [1.0 / 16.0, 1.0 / 4.0, 3.0 / 8.0, 1.0 / 4.0, 1.0 / 16.0];
    // Light per unit of surface colour, so textures stay sharp (not for
    // mirrors and glass, whose colour is what they reflect or let through).
    let tint = |f: &Feature| {
        if f.sharp {
            glam::Vec3::ONE
        } else {
            f.albedo.max(glam::Vec3::splat(0.15))
        }
    };
    let light = |c: [f32; 4], f: &Feature| {
        let a = tint(f);
        [c[0] / a.x, c[1] / a.y, c[2] / a.z, c[3]]
    };
    let cur: Vec<[f32; 4]> = image
        .iter()
        .zip(features)
        .map(|(c, f)| light(*c, f))
        .collect();
    let luma = |c: [f32; 4]| {
        let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        l / (1.0 + l)
    };
    // Fireflies: a pixel far brighter than all its neighbours on the same
    // surface takes their average instead.
    let src = &cur;
    let mut cur = rows(w, h, |x, y| {
        let i = y * w + x;
        let (fp, cp) = (&features[i], src[i]);
        if fp.id == EMPTY_ID {
            return cp;
        }
        let (mut brightest, mut sum, mut n) = (0f32, [0f32; 4], 0f32);
        for (dx, dy) in [
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ] {
            let (xx, yy) = (x as isize + dx, y as isize + dy);
            if xx < 0 || yy < 0 || xx >= w as isize || yy >= h as isize {
                continue;
            }
            let j = yy as usize * w + xx as usize;
            if features[j].id != fp.id {
                continue;
            }
            brightest = brightest.max(luma(src[j]));
            for k in 0..4 {
                sum[k] += src[j][k];
            }
            n += 1.0;
        }
        if n >= 3.0 && luma(cp) > brightest * 1.5 + 0.03 {
            sum.map(|v| v / n)
        } else {
            cp
        }
    });
    // Noise shrinks with the square root of the samples.
    let mut sigma = 0.8 / (samples.max(1) as f32).sqrt();
    for pass in 0..5 {
        let step = 1usize << pass;
        let src = &cur;
        let next = rows(w, h, |x, y| {
            let i = y * w + x;
            let (fp, cp) = (&features[i], src[i]);
            if fp.id == EMPTY_ID || (fp.sharp && pass > 2) {
                return cp;
            }
            let sigma = if fp.sharp { sigma * 0.5 } else { sigma };
            let lp = luma(cp);
            let mut sum = [0f32; 4];
            let mut total = 0f32;
            for (dy, ky) in KERNEL.iter().enumerate() {
                let yy = y as isize + (dy as isize - 2) * step as isize;
                if yy < 0 || yy >= h as isize {
                    continue;
                }
                for (dx, kx) in KERNEL.iter().enumerate() {
                    let xx = x as isize + (dx as isize - 2) * step as isize;
                    if xx < 0 || xx >= w as isize {
                        continue;
                    }
                    let j = yy as usize * w + xx as usize;
                    let fq = &features[j];
                    if fq.id != fp.id {
                        continue;
                    }
                    let cq = src[j];
                    let wn = fp.normal.dot(fq.normal).max(0.0).powi(32);
                    let wd = (-(fp.depth - fq.depth).abs()
                        / (0.02 * fp.depth.max(0.01) * step as f32))
                        .exp();
                    let wa = (-(fp.albedo - fq.albedo).length_squared() * 60.0).exp();
                    let wc = (-(lp - luma(cq)).abs() / sigma).exp();
                    let weight = kx * ky * wn * wd * wa * wc;
                    for k in 0..4 {
                        sum[k] += cq[k] * weight;
                    }
                    total += weight;
                }
            }
            if total > 0.0 {
                sum.map(|v| v / total)
            } else {
                cp
            }
        });
        cur = next;
        sigma *= 0.6;
    }
    cur.iter()
        .zip(features)
        .map(|(c, f)| {
            let a = tint(f);
            [c[0] * a.x, c[1] * a.y, c[2] * a.z, c[3]]
        })
        .collect()
}

/// Glow: light brighter than white bleeds into its surroundings, like
/// the viewport's bloom on emissive surfaces (premultiplied RGBA in/out).
pub(crate) fn bloom(image: &[[f32; 4]], w: usize, h: usize) -> Vec<[f32; 4]> {
    let mut glow: Vec<[f32; 4]> = image
        .iter()
        .map(|c| {
            let over = [c[0] - 1.0, c[1] - 1.0, c[2] - 1.0].map(|v| v.max(0.0));
            [over[0], over[1], over[2], 0.0]
        })
        .collect();
    if glow.iter().all(|g| g[0] + g[1] + g[2] == 0.0) {
        return image.to_vec();
    }
    let r = (w.max(h) / 64).max(2);
    for _ in 0..3 {
        blur(&mut glow, w, h, r);
    }
    image
        .iter()
        .zip(&glow)
        .map(|(c, g)| {
            let add = [g[0] * 0.6, g[1] * 0.6, g[2] * 0.6];
            let a = (c[3] + lum(DVec3::new(add[0] as f64, add[1] as f64, add[2] as f64)) as f32)
                .min(1.0);
            [c[0] + add[0], c[1] + add[1], c[2] + add[2], a.max(c[3])]
        })
        .collect()
}

/// Separable box blur, in place.
fn blur(img: &mut [[f32; 4]], w: usize, h: usize, r: usize) {
    let width = (2 * r + 1) as f32;
    for pass in 0..2 {
        let (lines, len) = if pass == 0 { (h, w) } else { (w, h) };
        let mut line = vec![[0f32; 4]; len];
        for a in 0..lines {
            let at = |b: usize| if pass == 0 { a * w + b } else { b * w + a };
            let mut sum = [0f32; 4];
            for b in 0..=r.min(len - 1) {
                for k in 0..4 {
                    sum[k] += img[at(b)][k];
                }
            }
            for (b, slot) in line.iter_mut().enumerate() {
                *slot = sum.map(|v| v / width);
                if b + r + 1 < len {
                    for k in 0..4 {
                        sum[k] += img[at(b + r + 1)][k];
                    }
                }
                if b >= r {
                    for k in 0..4 {
                        sum[k] -= img[at(b - r)][k];
                    }
                }
            }
            for (b, v) in line.iter().enumerate() {
                img[at(b)] = *v;
            }
        }
    }
}

/// Tone-map like the viewport (three.js's ACES filmic) into straight-alpha
/// sRGB bytes.
fn bytes(image: &[[f32; 4]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(image.len() * 4);
    for &c in image {
        let a = c[3].clamp(0.0, 1.0);
        let rgb = if a > 1e-6 {
            aces(DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64) / a as f64)
        } else {
            DVec3::ZERO
        };
        for v in [rgb.x, rgb.y, rgb.z] {
            out.push((crate::render::linear_to_srgb(v) * 255.0).round() as u8);
        }
        out.push((a * 255.0).round() as u8);
    }
    out
}

/// Composite straight sRGB RGBA over the viewport's backdrop (its CSS
/// radial gradient: #4a4c53, #303238 at 48%, #1b1c20; an ellipse 120% by
/// 90% around 50%, 38%), leaving it opaque.
fn backdrop(pixels: &mut [u8], w: usize, h: usize) {
    let stops = [
        (0.0, [0x4a, 0x4c, 0x53]),
        (0.48, [0x30, 0x32, 0x38]),
        (1.0, [0x1b, 0x1c, 0x20]),
    ];
    for y in 0..h {
        for x in 0..w {
            let dx = (x as f64 + 0.5 - 0.5 * w as f64) / (1.2 * w as f64);
            let dy = (y as f64 + 0.5 - 0.38 * h as f64) / (0.9 * h as f64);
            let d = (dx * dx + dy * dy).sqrt().min(1.0);
            let k = stops.windows(2).position(|s| d <= s[1].0).unwrap_or(1);
            let (a, b) = (stops[k], stops[k + 1]);
            let t = (d - a.0) / (b.0 - a.0);
            let i = (y * w + x) * 4;
            let alpha = pixels[i + 3] as f64 / 255.0;
            for c in 0..3 {
                let bg = a.1[c] as f64 + (b.1[c] as f64 - a.1[c] as f64) * t;
                pixels[i + c] = (pixels[i + c] as f64 * alpha + bg * (1.0 - alpha)).round() as u8;
            }
            pixels[i + 3] = 255;
        }
    }
}

/// Encode RGBA frames as a PNG, or an animated PNG (looping, at `fps`)
/// when there are several.
pub fn png(
    frames: &[Vec<u8>],
    w: usize,
    h: usize,
    transparent: bool,
    fps: f64,
) -> Result<Vec<u8>, EngineError> {
    let fail = |e: png::EncodingError| EngineError::new(e.to_string());
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, w as u32, h as u32);
        encoder.set_color(if transparent {
            png::ColorType::Rgba
        } else {
            png::ColorType::Rgb
        });
        encoder.set_depth(png::BitDepth::Eight);
        if frames.len() > 1 {
            encoder.set_animated(frames.len() as u32, 0).map_err(fail)?;
            encoder
                .set_frame_delay(1000, (fps * 1000.0).round().clamp(1.0, 65535.0) as u16)
                .map_err(fail)?;
        }
        let mut writer = encoder.write_header().map_err(fail)?;
        for frame in frames {
            if transparent {
                writer.write_image_data(frame).map_err(fail)?;
            } else {
                let rgb: Vec<u8> = frame.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
                writer.write_image_data(&rgb).map_err(fail)?;
            }
        }
        writer.finish().map_err(fail)?;
    }
    Ok(out)
}

/// three.js's ACES filmic tone mapping (exposure 1).
pub(crate) fn aces(c: DVec3) -> DVec3 {
    let c = c / 0.6;
    let i = glam::DMat3::from_cols(
        DVec3::new(0.59719, 0.07600, 0.02840),
        DVec3::new(0.35458, 0.90834, 0.13383),
        DVec3::new(0.04823, 0.01566, 0.83777),
    );
    let o = glam::DMat3::from_cols(
        DVec3::new(1.60475, -0.10208, -0.00327),
        DVec3::new(-0.53108, 1.10813, -0.07276),
        DVec3::new(-0.07367, -0.00605, 1.07602),
    );
    let v = i * c;
    let fit = |x: f64| {
        (x * (x + 0.024_578_6) - 0.000_090_537) / (x * (0.983_729 * x + 0.432_951) + 0.238_081)
    };
    (o * DVec3::new(fit(v.x), fit(v.y), fit(v.z))).clamp(DVec3::ZERO, DVec3::ONE)
}

/// A small fast generator (PCG-style hash stepping).
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        Self(seed | 1)
    }

    /// Uniform in [0, 1).
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
        let w = ((self.0 >> ((self.0 >> 28) + 4)) ^ self.0).wrapping_mul(277_803_737);
        ((w >> 22) ^ w) as f64 / 4_294_967_296.0
    }
}

fn hash3(x: u32, y: u32, z: u32) -> u32 {
    let mut h =
        x.wrapping_mul(0x8da6_b343) ^ y.wrapping_mul(0xd816_3841) ^ z.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CommandBatch;

    fn editor(commands: serde_json::Value) -> Editor {
        let mut ed = Editor::new();
        let batch: CommandBatch =
            serde_json::from_value(serde_json::json!({ "commands": commands })).unwrap();
        ed.apply(&batch).unwrap();
        ed
    }

    fn camera(w: usize, h: usize) -> Camera {
        Camera::new(
            DVec3::new(0.0, 1.6, 4.0),
            DVec3::new(0.0, 0.5, 0.0),
            36.0,
            w,
            h,
        )
    }

    #[test]
    fn hierarchy_finds_the_same_hits_as_brute_force() {
        let ed = editor(serde_json::json!([
            {"op": "add", "primitive": {"kind": "sphere", "radius": 0.5, "segments": 24, "rings": 12}, "translation": [0, 0.5, 0]},
            {"op": "add", "primitive": {"kind": "torus"}, "translation": [1, 0.4, -0.5]}
        ]));
        let t = traced(&ed, None).unwrap();
        let mut rng = Rng::new(7);
        for _ in 0..400 {
            let o = DVec3::new(
                rng.next() * 6.0 - 3.0,
                rng.next() * 3.0,
                rng.next() * 6.0 - 3.0,
            );
            let d = (DVec3::new(0.3, 0.5, -0.2) + DVec3::new(rng.next(), rng.next(), rng.next())
                - o * 0.3)
                .normalize();
            let brute = t
                .packed
                .iter()
                .filter_map(|p| intersect(p, o, d).map(|(t, ..)| t))
                .fold(f64::INFINITY, f64::min);
            let fast = t.hit(o, d, f64::INFINITY).map_or(f64::INFINITY, |h| h.t);
            assert!(
                (brute - fast).abs() < 1e-9 || (brute.is_infinite() && fast.is_infinite()),
                "{brute} vs {fast}"
            );
        }
    }

    #[test]
    fn objects_cover_the_frame_and_cast_shadows_on_the_floor() {
        let ed = editor(serde_json::json!([
            {"op": "add", "primitive": {"kind": "cube"}, "translation": [0, 0.5, 0], "color": "#d0d0d0"}
        ]));
        let t = traced(&ed, None).unwrap();
        let cam = camera(48, 32);
        let img = t.render(&cam, 16, 1);
        let at = |x: usize, y: usize| img[y * 48 + x];
        // The cube fills the centre; the sky above is empty.
        assert!((at(24, 14)[3] - 1.0).abs() < 1e-6);
        assert!(at(24, 14)[0] > 0.02);
        assert_eq!(at(2, 1)[3], 0.0);
        // The cube's shadow falls left of it, away from the key light
        // (grid lines alone are fainter than 0.2).
        let shadowed = (16..30)
            .flat_map(|y| (0..24).map(move |x| (x, y)))
            .filter(|&(x, y)| (0.2..0.99).contains(&at(x, y)[3]))
            .count();
        assert!(shadowed > 10, "{shadowed}");
        // The same seed renders the same image.
        assert_eq!(img, t.render(&cam, 16, 1));
        assert_ne!(img, t.render(&cam, 16, 2));
    }

    #[test]
    fn the_lit_side_is_brighter_and_glass_lets_light_through() {
        let ed = editor(serde_json::json!([
            {"op": "add", "primitive": {"kind": "sphere", "radius": 0.5, "segments": 32, "rings": 16}, "translation": [0, 0.5, 0], "color": "#ffffff", "roughness": 0.9}
        ]));
        let t = traced(&ed, None).unwrap();
        let cam = Camera {
            eye: DVec3::new(0.0, 0.5, 3.0),
            ..camera(40, 40)
        };
        let cam = Camera {
            target: DVec3::new(0.0, 0.5, 0.0),
            ..cam
        };
        let img = t.render(&cam, 64, 3);
        let lum = |p: [f32; 4]| p[0] + p[1] + p[2];
        // The key light comes from +x: the right of the ball is brighter.
        assert!(lum(img[20 * 40 + 27]) > lum(img[20 * 40 + 13]) * 1.3);

        // Light through a glass ball is tinted but not stopped; a clay
        // ball blocks it.
        let through = |preset: &str| {
            let ed = editor(serde_json::json!([
                {"op": "add", "primitive": {"kind": "sphere", "radius": 0.5, "segments": 32, "rings": 16}, "translation": [0, 1.2, 0], "preset": preset}
            ]));
            let t = traced(&ed, None).unwrap();
            t.transmittance(DVec3::ZERO, DVec3::Y, f64::INFINITY)
        };
        assert!(through("glass").min_element() > 0.3, "{}", through("glass"));
        assert_eq!(through("clay"), DVec3::ZERO);
    }

    #[test]
    fn the_world_lights_the_scene_and_can_show_behind_it() {
        let ball = serde_json::json!({"op": "add", "primitive": {"kind": "sphere", "radius": 0.5, "segments": 32, "rings": 16}, "translation": [0, 0.5, 0], "color": "#ffffff", "roughness": 0.9});
        let cam = Camera {
            eye: DVec3::new(0.0, 0.5, 3.0),
            target: DVec3::new(0.0, 0.5, 0.0),
            ..camera(40, 40)
        };
        let lum = |p: [f32; 4]| p[0] + p[1] + p[2];
        // The daylight sun stands at azimuth 35 degrees: turned by -35 it
        // shines from +x, turned by 145 from -x.
        let side = |rotation: f64| {
            let ed = editor(
                serde_json::json!([ball, {"op": "world", "sky": "daylight", "rotation": rotation}]),
            );
            let img = traced(&ed, None).unwrap().render(&cam, 48, 5);
            (lum(img[20 * 40 + 27]), lum(img[20 * 40 + 13]))
        };
        let (right, left) = side(-35.0);
        assert!(right > left * 1.5, "{right} vs {left}");
        let (right, left) = side(145.0);
        assert!(left > right * 1.5, "{right} vs {left}");

        // Strength scales the light.
        let centre = |world: serde_json::Value| {
            let ed = editor(serde_json::json!([ball, world]));
            lum(traced(&ed, None).unwrap().render(&cam, 32, 5)[20 * 40 + 20])
        };
        let dim = centre(serde_json::json!({"op": "world", "sky": "overcast"}));
        let bright = centre(serde_json::json!({"op": "world", "sky": "overcast", "strength": 2}));
        assert!((bright / dim - 2.0).abs() < 0.1, "{dim} -> {bright}");

        // Behind the scene: transparent, or the world itself.
        let corner = |background: bool| {
            let ed = editor(
                serde_json::json!([ball, {"op": "world", "sky": "sunset", "background": background}]),
            );
            traced(&ed, None).unwrap().render(&cam, 4, 5)[40 + 1]
        };
        assert_eq!(corner(false)[3], 0.0);
        let sky = corner(true);
        assert_eq!(sky[3], 1.0);
        assert!(sky[0] > sky[2], "a warm sunset sky: {sky:?}");
    }
}
