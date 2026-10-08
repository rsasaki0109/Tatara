//! Headless software renderer, so agents can look at what they built.
//!
//! It rasterizes the evaluated scene on the CPU, with crease-aware normals, a
//! shadow-mapped key light, a ground grid for scale and outlines for
//! legibility. Several named views are tiled into one labelled PNG. No GPU or
//! browser is needed: the editor process answers `GET /api/render` directly.

use std::collections::HashMap;
use std::sync::Arc;

use glam::dcamera::rh::{proj::directx, view::look_at_mat4};
use glam::{DMat3, DMat4, DVec2, DVec3, DVec4};

use crate::engine::{Editor, EngineError};
use crate::image::Pixels;
use crate::nodes::Baked;
use crate::texture::{BoxProjection, Look, Texture};

const SUPERSAMPLE: usize = 2;
const SHADOW_SIZE: usize = 1024;
pub(crate) const FOV_DEGREES: f64 = 36.0;
const GROUND: u32 = u32::MAX - 1;
const EMPTY: u32 = u32::MAX;

#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub name: String,
    pub azimuth: f64,
    pub elevation: f64,
}

impl View {
    /// Named camera presets; azimuth 0 looks from +Z (the front).
    pub fn preset(name: &str) -> Option<View> {
        let (az, el) = match name {
            "front" => (0.0, 6.0),
            "back" => (180.0, 6.0),
            "right" => (90.0, 6.0),
            "left" => (-90.0, 6.0),
            "top" => (0.0, 89.0),
            "bottom" => (0.0, -89.0),
            "iso" => (35.0, 26.0),
            _ => return None,
        };
        Some(View {
            name: name.into(),
            azimuth: az,
            elevation: el,
        })
    }
}

#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub views: Vec<View>,
    /// Pixel size of each square tile.
    pub size: u32,
    /// Frame this object instead of the whole scene.
    pub focus: Option<u64>,
    /// Pose animated objects at this frame (static pose when `None`).
    pub frame: Option<f64>,
    /// Path trace with this many samples per pixel instead of rasterizing.
    pub samples: Option<u32>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            views: vec![View::preset("iso").unwrap()],
            size: 512,
            focus: None,
            frame: None,
            samples: None,
        }
    }
}

pub(crate) struct Tri {
    pub(crate) p: [DVec3; 3],
    pub(crate) n: [DVec3; 3],
    /// Texture coordinates in tiles (zero when untextured).
    pub(crate) uv: [DVec2; 3],
    /// World directions of growing u and v (zero when untextured).
    pub(crate) tangents: [DVec3; 2],
    pub(crate) object: u32,
}

/// A material's texture with its images decoded.
struct Textured {
    base: String,
    texture: Texture,
    image: Option<Arc<Pixels>>,
    normal_map: Option<Arc<Pixels>>,
    /// A node graph's tiles.
    baked: Option<Baked>,
}

impl Textured {
    fn look(&self) -> Look<'_> {
        Look {
            base: &self.base,
            texture: &self.texture,
            image: self.image.as_deref(),
            normal_map: self.normal_map.as_deref(),
            baked: self.baked.as_ref(),
        }
    }
}

/// What a surface is like at one point.
pub(crate) struct Point {
    pub(crate) albedo: DVec3,
    pub(crate) normal: DVec3,
    pub(crate) roughness: f64,
    pub(crate) metalness: f64,
}

/// Where a box-projected texture blends its three planes: the object's
/// space as scaled (texture metres) and its rotation back to the world.
struct Triplanar {
    to_local: DMat4,
    rotation: DMat3,
    /// Tiles per metre.
    repeat: f64,
}

pub(crate) struct Shading {
    color: DVec3,
    texture: Option<Textured>,
    triplanar: Option<Triplanar>,
    roughness: f64,
    metalness: f64,
    /// Linear emitted light, strength applied.
    pub(crate) emissive: DVec3,
    pub(crate) opacity: f64,
    pub(crate) transmission: f64,
}

impl Shading {
    /// Linear base colour at texture coordinate `uv` (in tiles).
    fn albedo(&self, uv: DVec2) -> DVec3 {
        match &self.texture {
            Some(t) => {
                let c = t.look().color(uv.x, uv.y);
                DVec3::from_array(c.map(srgb_to_linear))
            }
            None => self.color,
        }
    }

    /// The surface at world point `p` with normal `n`: blended over three
    /// planes for box-projected textures (no seams on curved surfaces), or
    /// from the triangle's UVs otherwise.
    pub(crate) fn at(&self, p: DVec3, n: DVec3, tangents: [DVec3; 2], uv: DVec2) -> Point {
        let (Some(t), Some(tp)) = (&self.texture, &self.triplanar) else {
            let rm = self
                .texture
                .as_ref()
                .and_then(|t| t.look().roughness_metalness(uv.x, uv.y));
            return self.point(self.albedo(uv), self.bend(n, tangents, uv), rm);
        };
        let look = t.look();
        let q = tp.to_local.transform_point3(p);
        let nq = tp.rotation.transpose() * n;
        let c = look.triplanar_color(q, nq, tp.repeat);
        let albedo = DVec3::from_array(c.map(srgb_to_linear));
        let rm = look.triplanar_roughness_metalness(q, nq, tp.repeat);
        if !look.bumpy() {
            return self.point(albedo, n, rm);
        }
        let tilt = look.triplanar_tilt(q, nq, tp.repeat, 1.0 / 512.0);
        self.point(albedo, (tp.rotation * (nq + tilt)).normalize_or(n), rm)
    }

    fn point(&self, albedo: DVec3, normal: DVec3, rm: Option<(f64, f64)>) -> Point {
        let (roughness, metalness) = rm.unwrap_or((self.roughness, self.metalness));
        Point {
            albedo,
            normal,
            roughness,
            metalness,
        }
    }

    fn bend(&self, n: DVec3, tangents: [DVec3; 2], uv: DVec2) -> DVec3 {
        let Some(t) = &self.texture else {
            return n;
        };
        let look = t.look();
        if !look.bumpy() || tangents[0] == DVec3::ZERO {
            return n;
        }
        let tu = (tangents[0] - n * n.dot(tangents[0])).normalize_or_zero();
        let tv =
            (tangents[1] - n * n.dot(tangents[1]) - tu * tu.dot(tangents[1])).normalize_or_zero();
        let b = look.normal(uv.x, uv.y, 1.0 / 512.0);
        (tu * b.x + tv * b.y + n * b.z).normalize_or(n)
    }

    /// What share of light passing through is left (per channel): glass
    /// tints it, translucency thins it, opaque surfaces stop it.
    pub(crate) fn shadow_tint(&self) -> DVec3 {
        DVec3::splat(1.0 - self.opacity)
            + DVec3::ONE.lerp(self.color, 0.6) * (0.85 * self.transmission * self.opacity)
    }

    /// Drawn in the blended pass instead of the opaque one.
    fn transparent(&self) -> bool {
        self.opacity < 0.999 || self.transmission > 0.0
    }

    /// Mostly-see-through surfaces cast no shadow.
    fn casts_shadow(&self) -> bool {
        self.opacity * (1.0 - 0.9 * self.transmission) >= 0.5
    }
}

pub(crate) struct Prepared {
    pub(crate) tris: Vec<Tri>,
    pub(crate) materials: Vec<Shading>,
    pub(crate) center: DVec3,
    pub(crate) radius: f64,
    ground_y: f64,
}

fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub(crate) fn linear_to_srgb(c: f64) -> f64 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

pub(crate) fn hex(c: &str) -> DVec3 {
    let ch =
        |i: usize| srgb_to_linear(u8::from_str_radix(&c[i..i + 2], 16).unwrap_or(0) as f64 / 255.0);
    DVec3::new(ch(1), ch(3), ch(5))
}

/// ACES filmic approximation (Narkowicz), applied per channel.
pub(crate) fn tonemap(c: DVec3) -> DVec3 {
    let f = |x: f64| ((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)).clamp(0.0, 1.0);
    DVec3::new(f(c.x), f(c.y), f(c.z))
}

pub(crate) fn prepare(
    ed: &Editor,
    focus: Option<u64>,
    frame: Option<f64>,
) -> Result<Prepared, EngineError> {
    let mut tris = Vec::new();
    let mut materials = Vec::new();
    let mut decoded: HashMap<&str, Option<Arc<Pixels>>> = HashMap::new();
    let images = &ed.scene().images;
    let mut pixels = |name: &Option<String>| -> Option<Arc<Pixels>> {
        let name = name.as_deref()?;
        let (key, image) = images.get_key_value(name)?;
        decoded
            .entry(key.as_str())
            .or_insert_with(|| image.decode().ok().map(Arc::new))
            .clone()
    };
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    let (mut flo, mut fhi) = (lo, hi);
    for o in &ed.scene().objects {
        let (transform, material) = match frame {
            Some(f) => crate::anim::pose(o, f),
            None => (o.transform.clone(), o.material.clone()),
        };
        let m = transform.matrix();
        let normal_m = m.inverse().transpose();
        let mesh = ed.evaluated(o);
        let projection = material
            .texture
            .as_ref()
            .map(|t| BoxProjection::new(&mesh.vertices, transform.scale, t.fit));
        let shaded = crate::gltf::shade(mesh, o.smooth, projection.as_ref());
        let tile = match &material.texture {
            Some(t) if !shaded.tiled => t.scale,
            _ => 1.0,
        };
        let uvs: Vec<DVec2> = shaded
            .uvs
            .iter()
            .map(|[u, v]| DVec2::new(*u as f64, *v as f64) / tile)
            .collect();
        let index = materials.len() as u32;
        let texture = material.texture.clone().map(|t| Textured {
            base: material.color.clone(),
            baked: crate::nodes::bake_material(&material, images, 512),
            image: pixels(&t.image),
            normal_map: pixels(&t.normal_map),
            texture: t,
        });
        // Box projection blends its planes per pixel; own and fitted UVs
        // are used as they are.
        let triplanar = (texture.is_some() && !shaded.tiled).then(|| {
            let s = DVec3::from_array(transform.scale);
            let safe = |x: f64| if x.abs() < 1e-9 { 1e-9 } else { x };
            Triplanar {
                to_local: DMat4::from_scale(s) * m.inverse(),
                rotation: DMat3::from_mat4(m)
                    * DMat3::from_diagonal(DVec3::new(
                        1.0 / safe(s.x),
                        1.0 / safe(s.y),
                        1.0 / safe(s.z),
                    )),
                repeat: 1.0 / tile,
            }
        });
        materials.push(Shading {
            color: hex(&material.color),
            texture,
            triplanar,
            roughness: material.roughness,
            metalness: material.metalness,
            emissive: hex(&material.emissive) * material.emissive_strength,
            opacity: material.opacity,
            transmission: material.transmission,
        });
        let world: Vec<DVec3> = shaded
            .positions
            .iter()
            .map(|p| m.transform_point3(DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)))
            .collect();
        let normals: Vec<DVec3> = shaded
            .normals
            .iter()
            .map(|n| {
                normal_m
                    .transform_vector3(DVec3::new(n[0] as f64, n[1] as f64, n[2] as f64))
                    .normalize_or_zero()
            })
            .collect();
        for p in &world {
            lo = lo.min(*p);
            hi = hi.max(*p);
            if focus == Some(o.id) {
                flo = flo.min(*p);
                fhi = fhi.max(*p);
            }
        }
        for t in shaded.indices.as_chunks::<3>().0 {
            let [a, b, c] = t.map(|i| i as usize);
            let p = [world[a], world[b], world[c]];
            let uv = if uvs.is_empty() {
                [DVec2::ZERO; 3]
            } else {
                [uvs[a], uvs[b], uvs[c]]
            };
            tris.push(Tri {
                p,
                n: [normals[a], normals[b], normals[c]],
                uv,
                tangents: tangents(p, uv),
                object: index,
            });
        }
    }
    if let Some(id) = focus {
        if !ed.scene().objects.iter().any(|o| o.id == id) {
            return Err(EngineError::new(format!("no object with id {id}")));
        }
        if flo.x.is_finite() {
            lo = flo;
            hi = fhi;
        }
    }
    let (center, radius) = if lo.x.is_finite() {
        ((lo + hi) * 0.5, ((hi - lo).length() * 0.5).max(0.05))
    } else {
        (DVec3::new(0.0, 0.5, 0.0), 1.5)
    };
    let scene_min_y = tris
        .iter()
        .flat_map(|t| t.p)
        .map(|p| p.y)
        .fold(0.0, f64::min);
    Ok(Prepared {
        tris,
        materials,
        center,
        radius,
        ground_y: scene_min_y.min(0.0),
    })
}

/// World-space directions in which u and v grow across a triangle.
fn tangents(p: [DVec3; 3], uv: [DVec2; 3]) -> [DVec3; 2] {
    let (e1, e2) = (p[1] - p[0], p[2] - p[0]);
    let (d1, d2) = (uv[1] - uv[0], uv[2] - uv[0]);
    let det = d1.x * d2.y - d2.x * d1.y;
    if det.abs() < 1e-12 {
        return [DVec3::ZERO; 2];
    }
    [(e1 * d2.y - e2 * d1.y) / det, (e2 * d1.x - e1 * d2.x) / det]
}

/// Rasterize one clip-space triangle with near-plane clipping. `pixel` gets
/// (x, y, depth in 0..1, perspective-correct barycentrics of the original).
fn raster(
    clip: [DVec4; 3],
    w: usize,
    h: usize,
    mut pixel: impl FnMut(usize, usize, f64, [f64; 3]),
) {
    // Sutherland-Hodgman against z >= 0 (glam's 0..1 depth range).
    let mut poly: Vec<(DVec4, [f64; 3])> = vec![
        (clip[0], [1.0, 0.0, 0.0]),
        (clip[1], [0.0, 1.0, 0.0]),
        (clip[2], [0.0, 0.0, 1.0]),
    ];
    if clip.iter().any(|c| c.z < 0.0) {
        let mut out = Vec::with_capacity(4);
        for i in 0..poly.len() {
            let (a, wa) = poly[i];
            let (b, wb) = poly[(i + 1) % poly.len()];
            let (ina, inb) = (a.z >= 0.0, b.z >= 0.0);
            if ina {
                out.push((a, wa));
            }
            if ina != inb {
                let t = a.z / (a.z - b.z);
                let lerp = |x: f64, y: f64| x + (y - x) * t;
                out.push((
                    a + (b - a) * t,
                    [lerp(wa[0], wb[0]), lerp(wa[1], wb[1]), lerp(wa[2], wb[2])],
                ));
            }
        }
        poly = out;
    }
    if poly.len() < 3 {
        return;
    }
    let screen: Vec<(f64, f64, f64, f64)> = poly
        .iter()
        .map(|(c, _)| {
            let iw = 1.0 / c.w.max(1e-9);
            (
                (c.x * iw * 0.5 + 0.5) * w as f64,
                (0.5 - c.y * iw * 0.5) * h as f64,
                c.z * iw,
                iw,
            )
        })
        .collect();
    for k in 1..poly.len() - 1 {
        let idx = [0, k, k + 1];
        let s = idx.map(|i| screen[i]);
        let area = (s[1].0 - s[0].0) * (s[2].1 - s[0].1) - (s[1].1 - s[0].1) * (s[2].0 - s[0].0);
        if area.abs() < 1e-12 {
            continue;
        }
        let x0 = s
            .iter()
            .map(|p| p.0)
            .fold(f64::INFINITY, f64::min)
            .floor()
            .max(0.0) as usize;
        let x1 = (s
            .iter()
            .map(|p| p.0)
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil() as isize)
            .min(w as isize - 1);
        let y0 = s
            .iter()
            .map(|p| p.1)
            .fold(f64::INFINITY, f64::min)
            .floor()
            .max(0.0) as usize;
        let y1 = (s
            .iter()
            .map(|p| p.1)
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil() as isize)
            .min(h as isize - 1);
        if x1 < 0 || y1 < 0 {
            continue;
        }
        for y in y0..=y1 as usize {
            let py = y as f64 + 0.5;
            for x in x0..=x1 as usize {
                let px = x as f64 + 0.5;
                let e = |a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)| {
                    (b.0 - a.0) * (py - a.1) - (b.1 - a.1) * (px - a.0)
                };
                let b0 = e(s[1], s[2]) / area;
                let b1 = e(s[2], s[0]) / area;
                let b2 = e(s[0], s[1]) / area;
                if b0 < 0.0 || b1 < 0.0 || b2 < 0.0 {
                    continue;
                }
                let z = b0 * s[0].2 + b1 * s[1].2 + b2 * s[2].2;
                if !(0.0..=1.0).contains(&z) {
                    continue;
                }
                let (q0, q1, q2) = (b0 * s[0].3, b1 * s[1].3, b2 * s[2].3);
                let sum = q0 + q1 + q2;
                let (q0, q1, q2) = (q0 / sum, q1 / sum, q2 / sum);
                let wts = idx.map(|i| poly[i].1);
                let bary = [0, 1, 2].map(|j| q0 * wts[0][j] + q1 * wts[1][j] + q2 * wts[2][j]);
                pixel(x, y, z, bary);
            }
        }
    }
}

struct ShadowMap {
    vp: DMat4,
    depth: Vec<f32>,
    light: DVec3,
}

impl ShadowMap {
    fn build(prep: &Prepared) -> Self {
        let light = DVec3::new(4.5, 8.0, 3.5).normalize();
        let r = prep.radius.max(0.5) * 1.6;
        let view = look_at_mat4(prep.center + light * r * 4.0, prep.center, DVec3::Z);
        let proj = directx::orthographic(-r, r, -r, r, 0.01, r * 10.0);
        let vp = proj * view;
        let mut depth = vec![1.0f32; SHADOW_SIZE * SHADOW_SIZE];
        for t in &prep.tris {
            if !prep.materials[t.object as usize].casts_shadow() {
                continue;
            }
            let clip = t.p.map(|p| vp * p.extend(1.0));
            raster(clip, SHADOW_SIZE, SHADOW_SIZE, |x, y, z, _| {
                let d = &mut depth[y * SHADOW_SIZE + x];
                if (z as f32) < *d {
                    *d = z as f32;
                }
            });
        }
        Self { vp, depth, light }
    }

    /// 0 = fully shadowed, 1 = lit; 3x3 PCF.
    fn lit(&self, p: DVec3, n: DVec3) -> f64 {
        let c = self.vp * (p + n * 0.01).extend(1.0);
        let (u, v, z) = (
            (c.x * 0.5 + 0.5) * SHADOW_SIZE as f64,
            (0.5 - c.y * 0.5) * SHADOW_SIZE as f64,
            c.z,
        );
        let mut sum = 0.0;
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (x, y) = ((u as isize + dx), (v as isize + dy));
                let ok = x < 0
                    || y < 0
                    || x >= SHADOW_SIZE as isize
                    || y >= SHADOW_SIZE as isize
                    || z - 0.0015 <= self.depth[y as usize * SHADOW_SIZE + x as usize] as f64;
                sum += if ok { 1.0 } else { 0.0 };
            }
        }
        sum / 9.0
    }
}

/// Where a view's camera stands (it looks at the scene's centre), and how
/// far away that is.
fn view_eye(prep: &Prepared, view: &View) -> (DVec3, f64) {
    let fov = FOV_DEGREES.to_radians();
    let distance = prep.radius * 1.12 / (fov / 2.0).sin();
    let (az, el) = (
        view.azimuth.to_radians(),
        view.elevation.clamp(-89.5, 89.5).to_radians(),
    );
    let dir = DVec3::new(el.cos() * az.sin(), el.sin(), el.cos() * az.cos());
    (prep.center + dir * distance, distance)
}

/// A path-traced tile over the same backdrop as the rasterized ones.
fn traced_tile(
    traced: &crate::pathtrace::Traced,
    eye: DVec3,
    target: DVec3,
    view: &View,
    size: usize,
    samples: u32,
) -> Vec<[u8; 3]> {
    let camera = crate::pathtrace::Camera {
        eye,
        target,
        fov: FOV_DEGREES,
        width: size,
        height: size,
    };
    let image = traced.develop(
        &traced.render(&camera, samples, 0),
        &traced.features(&camera),
        &camera,
        samples,
    );
    let (top, bottom) = (hex("#4a4c53"), hex("#1d1e22"));
    let mut pixels: Vec<[u8; 3]> = image
        .iter()
        .enumerate()
        .map(|(i, px)| {
            let bg = top.lerp(bottom, (i / size) as f64 / size as f64);
            let c =
                DVec3::new(px[0] as f64, px[1] as f64, px[2] as f64) + bg * (1.0 - px[3] as f64);
            let c = tonemap(c);
            [c.x, c.y, c.z].map(|v| (linear_to_srgb(v) * 255.0).round() as u8)
        })
        .collect();
    draw_text(&mut pixels, size, &view.name.to_uppercase(), 10, 10, 2);
    pixels
}

/// Render one square tile, returning sRGB bytes.
fn render_tile(prep: &Prepared, shadow: &ShadowMap, view: &View, size: usize) -> Vec<[u8; 3]> {
    let n = size * SUPERSAMPLE;
    let fov = FOV_DEGREES.to_radians();
    let (eye, distance) = view_eye(prep, view);
    let view_m = look_at_mat4(eye, prep.center, DVec3::Y);
    let near = (distance - prep.radius * 3.0).max(0.01);
    let proj = directx::perspective(fov, 1.0, near, distance + prep.radius * 20.0);
    let vp = proj * view_m;

    let mut depth = vec![f32::INFINITY; n * n];
    let mut id = vec![EMPTY; n * n];
    let mut normal = vec![DVec3::ZERO; n * n];
    let mut world = vec![DVec3::ZERO; n * n];
    let mut uv = vec![DVec2::ZERO; n * n];
    let mut tri_at = vec![u32::MAX; n * n];

    let g = prep.radius * 8.0 + 4.0;
    let gy = prep.ground_y;
    let c = prep.center;
    let ground = [
        DVec3::new(c.x - g, gy, c.z - g),
        DVec3::new(c.x + g, gy, c.z - g),
        DVec3::new(c.x + g, gy, c.z + g),
        DVec3::new(c.x - g, gy, c.z + g),
    ];
    let mut draw = |p: [DVec3; 3], nrm: [DVec3; 3], tuv: [DVec2; 3], object: u32, tri: u32| {
        let clip = p.map(|q| vp * q.extend(1.0));
        raster(clip, n, n, |x, y, z, b| {
            let i = y * n + x;
            if (z as f32) < depth[i] {
                depth[i] = z as f32;
                id[i] = object;
                normal[i] = (nrm[0] * b[0] + nrm[1] * b[1] + nrm[2] * b[2]).normalize_or_zero();
                world[i] = p[0] * b[0] + p[1] * b[1] + p[2] * b[2];
                uv[i] = tuv[0] * b[0] + tuv[1] * b[1] + tuv[2] * b[2];
                tri_at[i] = tri;
            }
        });
    };
    if view.elevation > -5.0 {
        draw(
            [ground[0], ground[2], ground[1]],
            [DVec3::Y; 3],
            [DVec2::ZERO; 3],
            GROUND,
            u32::MAX,
        );
        draw(
            [ground[0], ground[3], ground[2]],
            [DVec3::Y; 3],
            [DVec2::ZERO; 3],
            GROUND,
            u32::MAX,
        );
    }
    for (k, t) in prep.tris.iter().enumerate() {
        if !prep.materials[t.object as usize].transparent() {
            draw(t.p, t.n, t.uv, t.object, k as u32);
        }
    }

    let bg_top = hex("#4a4c53");
    let bg_bottom = hex("#1d1e22");
    let ground_col = hex("#34363c");
    let mut color = vec![DVec3::ZERO; n * n];
    for y in 0..n {
        let t = y as f64 / n as f64;
        let bg = bg_top.lerp(bg_bottom, t);
        for x in 0..n {
            let i = y * n + x;
            color[i] = match id[i] {
                EMPTY => bg,
                GROUND => {
                    let p = world[i];
                    let lit = 0.45 + 0.55 * shadow.lit(p, DVec3::Y);
                    let lw = distance * 0.0016;
                    let line = |v: f64| {
                        let f = v - v.round();
                        (1.0 - (f.abs() / lw).min(1.0)) * 0.6
                    };
                    let grid = line(p.x).max(line(p.z));
                    let base = ground_col * (1.0 + grid * 0.9) * lit;
                    let fade =
                        (1.0 - ((p - c).length() / (prep.radius * 6.0 + 3.0))).clamp(0.0, 1.0);
                    bg.lerp(base, fade)
                }
                o => {
                    let m = &prep.materials[o as usize];
                    let tangents = prep.tris[tri_at[i] as usize].tangents;
                    let pt = m.at(world[i], normal[i], tangents, uv[i]);
                    let (diffuse, gloss) = surface(m, &pt, world[i], eye, shadow);
                    diffuse + gloss + m.emissive
                }
            };
        }
    }

    // Outlines at object boundaries and sharp creases.
    let mut out = color.clone();
    for y in 0..n {
        for x in 0..n {
            let i = y * n + x;
            if id[i] == EMPTY || id[i] == GROUND {
                continue;
            }
            let mut edge: f64 = 0.0;
            for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                let (xx, yy) = (x as i32 + dx, y as i32 + dy);
                if xx < 0 || yy < 0 || xx >= n as i32 || yy >= n as i32 {
                    continue;
                }
                let j = yy as usize * n + xx as usize;
                if id[j] != id[i] {
                    edge = edge.max(1.0);
                } else if normal[i].dot(normal[j]) < 0.5 {
                    edge = edge.max(0.5);
                }
            }
            if edge > 0.0 {
                out[i] = color[i] * (1.0 - 0.6 * edge);
            }
        }
    }

    // Glass and translucent surfaces, back to front over the opaque image.
    let mut blended: Vec<&Tri> = prep
        .tris
        .iter()
        .filter(|t| prep.materials[t.object as usize].transparent())
        .collect();
    let far = |t: &Tri| (t.p[0] + t.p[1] + t.p[2]).distance_squared(eye * 3.0);
    blended.sort_by(|a, b| far(b).total_cmp(&far(a)));
    for t in blended {
        let m = &prep.materials[t.object as usize];
        let clip = t.p.map(|q| vp * q.extend(1.0));
        raster(clip, n, n, |x, y, z, b| {
            let i = y * n + x;
            if z as f32 >= depth[i] {
                return;
            }
            let p = t.p[0] * b[0] + t.p[1] * b[1] + t.p[2] * b[2];
            let nn = (t.n[0] * b[0] + t.n[1] * b[1] + t.n[2] * b[2]).normalize_or_zero();
            let at = t.uv[0] * b[0] + t.uv[1] * b[1] + t.uv[2] * b[2];
            let pt = m.at(p, nn, t.tangents, at);
            let albedo = pt.albedo;
            let (diffuse, gloss) = surface(m, &pt, p, eye, shadow);
            let facing = nn.dot((eye - p).normalize()).abs();
            let reflect = (1.0 - facing).powi(3);
            let tint = DVec3::ONE.lerp(albedo, 0.55) * m.transmission * (1.0 - reflect);
            let pass = DVec3::splat(1.0 - m.opacity) + tint * m.opacity;
            out[i] =
                out[i] * pass + (diffuse * (1.0 - m.transmission) + gloss + m.emissive) * m.opacity;
        });
    }

    // Bloom: emissive surfaces bleed light into their surroundings.
    let mut glow = vec![DVec3::ZERO; n * n];
    let mut glowing = false;
    for i in 0..n * n {
        if id[i] != EMPTY && id[i] != GROUND {
            let e = prep.materials[id[i] as usize].emissive;
            if e != DVec3::ZERO {
                glow[i] = e;
                glowing = true;
            }
        }
    }
    if glowing {
        let radius = (n / 48).max(2);
        for _ in 0..3 {
            box_blur(&mut glow, n, radius);
        }
        for (o, g) in out.iter_mut().zip(&glow) {
            *o += *g * 0.45;
        }
    }

    let mut pixels = vec![[0u8; 3]; size * size];
    for y in 0..size {
        for x in 0..size {
            let mut acc = DVec3::ZERO;
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    acc += out[(y * SUPERSAMPLE + sy) * n + x * SUPERSAMPLE + sx];
                }
            }
            let c = tonemap(acc / (SUPERSAMPLE * SUPERSAMPLE) as f64);
            pixels[y * size + x] =
                [c.x, c.y, c.z].map(|v| (linear_to_srgb(v) * 255.0).round() as u8);
        }
    }
    draw_text(&mut pixels, size, &view.name.to_uppercase(), 10, 10, 2);
    pixels
}

/// Separable box blur of a square image, in place.
fn box_blur(img: &mut [DVec3], n: usize, r: usize) {
    let mut line = vec![DVec3::ZERO; n];
    let width = (2 * r + 1) as f64;
    for pass in 0..2 {
        for a in 0..n {
            let at = |b: usize| if pass == 0 { a * n + b } else { b * n + a };
            let mut sum = DVec3::ZERO;
            for b in 0..=r.min(n - 1) {
                sum += img[at(b)];
            }
            for (b, slot) in line.iter_mut().enumerate() {
                *slot = sum / width;
                if b + r + 1 < n {
                    sum += img[at(b + r + 1)];
                }
                if b >= r {
                    sum -= img[at(b - r)];
                }
            }
            for (b, v) in line.iter().enumerate() {
                img[at(b)] = *v;
            }
        }
    }
}

/// Diffuse and glossy light leaving a surface point toward the eye; `pt`
/// is the (possibly textured) surface there.
fn surface(m: &Shading, pt: &Point, p: DVec3, eye: DVec3, shadow: &ShadowMap) -> (DVec3, DVec3) {
    let (albedo, n) = (pt.albedo, pt.normal);
    let v = (eye - p).normalize();
    let nn = if n.dot(v) < 0.0 { -n } else { n };
    let l = shadow.light;
    let rim = DVec3::new(-5.0, 4.0, -6.0).normalize();
    let lit = shadow.lit(p, nn);
    let diffuse_col = albedo * (1.0 - pt.metalness * 0.85);
    let hemi = DVec3::new(0.30, 0.31, 0.34).lerp(DVec3::new(0.62, 0.64, 0.70), 0.5 + 0.5 * nn.y);
    let key = nn.dot(l).max(0.0) * lit * 2.0;
    let fill = nn.dot(rim).max(0.0) * 0.55;
    let h = (l + v).normalize();
    let shininess = (2.0 / pt.roughness.max(0.05).powi(4) - 2.0).clamp(2.0, 2048.0);
    let spec_strength = (1.0 - pt.roughness).powi(2) * 0.9 + 0.04;
    let spec_col = DVec3::splat(1.0).lerp(albedo, pt.metalness);
    let spec =
        nn.dot(h).max(0.0).powf(shininess) * spec_strength * lit * 2.3 * (shininess + 8.0) / 64.0;
    let grazing = 1.0 - nn.dot(v).max(0.0);
    let fresnel = grazing.powi(5) * 0.25;
    // Glass mirrors the studio at grazing angles; metals mirror it everywhere.
    let sheen = hemi * (0.04 + 0.9 * grazing.powi(3)) * m.transmission
        + spec_col * hemi * pt.metalness * (1.0 - pt.roughness * 0.6) * 0.55;
    (
        diffuse_col * (hemi * 0.9 + DVec3::splat(key + fill)),
        spec_col * (spec + fresnel) + sheen,
    )
}

// 5x7 bitmap font for tile labels.
fn glyph(c: char) -> [u8; 7] {
    match c {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        _ => [0; 7],
    }
}

pub(crate) fn draw_text(
    pixels: &mut [[u8; 3]],
    size: usize,
    text: &str,
    x0: usize,
    y0: usize,
    scale: usize,
) {
    for (pass, color, off) in [(0, [20u8, 21, 24], 1usize), (1, [240, 238, 232], 0)] {
        let _ = pass;
        for (ci, ch) in text.chars().enumerate() {
            let rows = glyph(ch);
            for (ry, bits) in rows.iter().enumerate() {
                for rx in 0..5 {
                    if bits & (0x10 >> rx) == 0 {
                        continue;
                    }
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let x = x0 + off + (ci * 6 + rx) * scale + sx;
                            let y = y0 + off + ry * scale + sy;
                            if x < size && y < size {
                                pixels[y * size + x] = color;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Render the requested views into one PNG (tiles in a grid, two columns).
pub fn render_png(ed: &Editor, opts: &RenderOptions) -> Result<Vec<u8>, EngineError> {
    render_job(ed, opts)?.png()
}

/// A render with the scene already gathered, so it can run without the
/// editor (servers render while other requests go on).
pub struct RenderJob {
    prep: Prepared,
    opts: RenderOptions,
}

/// Check `opts` and gather what the render needs from `ed`.
pub fn render_job(ed: &Editor, opts: &RenderOptions) -> Result<RenderJob, EngineError> {
    if opts.views.is_empty() || opts.views.len() > 6 {
        return Err(EngineError::new("request between 1 and 6 views"));
    }
    if !(64..=1024).contains(&opts.size) {
        return Err(EngineError::new("size must be between 64 and 1024"));
    }
    if opts.samples.is_some_and(|n| !(1..=256).contains(&n)) {
        return Err(EngineError::new("samples must be between 1 and 256"));
    }
    Ok(RenderJob {
        prep: prepare(ed, opts.focus, opts.frame)?,
        opts: opts.clone(),
    })
}

impl RenderJob {
    pub fn png(self) -> Result<Vec<u8>, EngineError> {
        let RenderJob { prep, opts } = self;
        let size = opts.size as usize;
        let tiles: Vec<Vec<[u8; 3]>> = match opts.samples {
            Some(samples) => {
                let cameras: Vec<(DVec3, DVec3)> = opts
                    .views
                    .iter()
                    .map(|v| (view_eye(&prep, v).0, prep.center))
                    .collect();
                let traced = crate::pathtrace::Traced::new(prep);
                opts.views
                    .iter()
                    .zip(cameras)
                    .map(|(v, (eye, target))| traced_tile(&traced, eye, target, v, size, samples))
                    .collect()
            }
            None => {
                let shadow = ShadowMap::build(&prep);
                opts.views
                    .iter()
                    .map(|v| render_tile(&prep, &shadow, v, size))
                    .collect()
            }
        };
        tiled_png(&tiles, size)
    }
}

/// Lay square tiles out two per row into a PNG.
fn tiled_png(tiles: &[Vec<[u8; 3]>], size: usize) -> Result<Vec<u8>, EngineError> {
    let cols = if tiles.len() == 1 { 1 } else { 2 };
    let rows = tiles.len().div_ceil(cols);
    let gap = 2;
    let (w, h) = (
        cols * size + (cols - 1) * gap,
        rows * size + (rows - 1) * gap,
    );
    let mut image = vec![12u8; w * h * 3];
    for (k, tile) in tiles.iter().enumerate() {
        let (ox, oy) = ((k % cols) * (size + gap), (k / cols) * (size + gap));
        for y in 0..size {
            for x in 0..size {
                let at = ((oy + y) * w + ox + x) * 3;
                image[at..at + 3].copy_from_slice(&tile[y * size + x]);
            }
        }
    }
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, w as u32, h as u32);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| EngineError::new(e.to_string()))?;
        writer
            .write_image_data(&image)
            .map_err(|e| EngineError::new(e.to_string()))?;
    }
    Ok(png_bytes)
}

/// Parse a comma-separated list of preset names and `az:el` pairs.
pub fn parse_views(spec: &str) -> Result<Vec<View>, EngineError> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            if let Some(v) = View::preset(s) {
                return Ok(v);
            }
            let (a, e) = s.split_once(':').ok_or_else(|| EngineError::new(format!("unknown view {s:?}: use front, back, left, right, top, bottom, iso or azimuth:elevation")))?;
            let (az, el) = (a.trim().parse::<f64>(), e.trim().parse::<f64>());
            match (az, el) {
                (Ok(az), Ok(el)) if az.is_finite() && el.is_finite() => Ok(View { name: format!("az {az:.0} el {el:.0}"), azimuth: az, elevation: el }),
                _ => Err(EngineError::new(format!("invalid view {s:?}"))),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CommandBatch;

    fn decode(png_bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        (info.width, info.height, buf[..info.buffer_size()].to_vec())
    }

    fn scene() -> Editor {
        let mut ed = Editor::new();
        let batch: CommandBatch = serde_json::from_value(serde_json::json!({"commands": [
            {"op": "add", "name": "Red", "primitive": {"kind": "cube"}, "translation": [-0.8, 0.5, 0], "color": "#c83232"},
            {"op": "add", "name": "Blue", "primitive": {"kind": "sphere", "segments": 16, "rings": 8}, "translation": [0.8, 0.5, 0], "color": "#3250c8"}
        ]}))
        .unwrap();
        ed.apply(&batch).unwrap();
        ed
    }

    #[test]
    fn renders_objects_where_they_are() {
        let ed = scene();
        let opts = RenderOptions {
            views: parse_views("front").unwrap(),
            size: 96,
            focus: None,
            frame: None,
            samples: None,
        };
        let (w, h, px) = decode(&render_png(&ed, &opts).unwrap());
        assert_eq!((w, h), (96, 96));
        // From the front, the red cube is on the left and the blue sphere on the right.
        let sample = |x: usize, y: usize| &px[(y * 96 + x) * 3..(y * 96 + x) * 3 + 3];
        let (left, right) = (sample(30, 52), sample(66, 52));
        assert!(left[0] > left[2] + 30, "left is red: {left:?}");
        assert!(right[2] > right[0] + 30, "right is blue: {right:?}");
    }

    #[test]
    fn samples_textures() {
        let dark_pixels = |texture: serde_json::Value| {
            let mut ed = Editor::new();
            let batch: CommandBatch = serde_json::from_value(serde_json::json!({"commands": [
                {"op": "add", "primitive": {"kind": "cube"}, "translation": [0, 0.5, 0], "color": "#f0f0f0", "texture": texture}
            ]}))
            .unwrap();
            ed.apply(&batch).unwrap();
            let opts = RenderOptions {
                views: parse_views("front").unwrap(),
                size: 64,
                focus: None,
                frame: None,
                samples: None,
            };
            let (_, _, px) = decode(&render_png(&ed, &opts).unwrap());
            px.chunks(3).filter(|c| c.iter().all(|&v| v < 40)).count()
        };
        let plain = dark_pixels(serde_json::Value::Null);
        let checker = dark_pixels(
            serde_json::json!({"pattern": "checker", "color2": "#000000", "scale": 0.5}),
        );
        assert!(
            checker > plain + 300,
            "dark squares show: {plain} vs {checker}"
        );
    }

    #[test]
    fn samples_images_and_bends_light_with_relief() {
        use base64::Engine as _;
        let png = base64::engine::general_purpose::STANDARD.encode(crate::image::tests::tiny_png());
        let render = |texture: serde_json::Value| {
            let mut ed = Editor::new();
            let batch: CommandBatch = serde_json::from_value(serde_json::json!({"commands": [
                {"op": "add_image", "name": "Quads", "data": png},
                {"op": "add", "primitive": {"kind": "cube"}, "translation": [0, 0.5, 0], "color": "#ffffff", "texture": texture}
            ]}))
            .unwrap();
            ed.apply(&batch).unwrap();
            let opts = RenderOptions {
                views: parse_views("front").unwrap(),
                size: 64,
                focus: None,
                frame: None,
                samples: None,
            };
            decode(&render_png(&ed, &opts).unwrap()).2
        };
        let image = render(serde_json::json!({"pattern": "image", "image": "Quads", "scale": 1}));
        let has = |px: &[u8], f: &dyn Fn(&[u8]) -> bool| px.chunks(3).filter(|c| f(c)).count();
        assert!(
            has(&image, &|c| c[0] > 120 && c[1] < 60 && c[2] < 60) > 50,
            "red quarter"
        );
        assert!(
            has(&image, &|c| c[2] as i32 > c[0] as i32 + 40
                && c[2] as i32 > c[1] as i32 + 40)
                > 50,
            "blue quarter"
        );
        let flat = render(serde_json::json!({"pattern": "brick", "color2": "#ffffff"}));
        let bumpy =
            render(serde_json::json!({"pattern": "brick", "color2": "#ffffff", "relief": 1}));
        let changed = flat
            .iter()
            .zip(&bumpy)
            .filter(|(a, b)| a.abs_diff(**b) > 8)
            .count();
        assert!(
            changed > 30,
            "relief shades the mortar edges ({changed} values changed)"
        );
    }

    #[test]
    fn renders_node_graph_colours_and_metal() {
        let render = |output: serde_json::Value| {
            let mut ed = Editor::new();
            let batch: CommandBatch = serde_json::from_value(serde_json::json!({"commands": [
                {"op": "add", "primitive": {"kind": "cube"}, "translation": [0, 0.5, 0], "color": "#808080", "roughness": 0.9, "texture": {"pattern": "nodes", "graph": {
                    "nodes": [{"id": "c", "type": "voronoi", "scale": 3, "output": "cells"},
                              {"id": "r", "type": "ramp", "factor": {"node": "c"}, "constant": true, "stops": [{"at": 0, "color": "#d03020"}, {"at": 0.5, "color": "#2040d0"}]}],
                    "output": output
                }}}
            ]}))
            .unwrap();
            ed.apply(&batch).unwrap();
            let opts = RenderOptions {
                views: parse_views("front").unwrap(),
                size: 64,
                focus: None,
                frame: None,
                samples: None,
            };
            decode(&render_png(&ed, &opts).unwrap()).2
        };
        let px = render(serde_json::json!({"color": {"node": "r"}}));
        let count = |px: &[u8], f: &dyn Fn(&[u8]) -> bool| px.chunks(3).filter(|c| f(c)).count();
        assert!(
            count(&px, &|c| c[0] as i32 > c[2] as i32 + 50) > 100,
            "red cells"
        );
        assert!(
            count(&px, &|c| c[2] as i32 > c[0] as i32 + 50) > 100,
            "blue cells"
        );
        let shiny = render(serde_json::json!({"color": {"node": "r"}, "roughness": 0.05}));
        assert_ne!(px, shiny, "the graph's roughness changes the highlight");
    }

    #[test]
    fn tiles_views_into_a_grid() {
        let ed = scene();
        let opts = RenderOptions {
            views: parse_views("front, right, top, 45:30").unwrap(),
            size: 64,
            focus: None,
            frame: None,
            samples: None,
        };
        let (w, h, _) = decode(&render_png(&ed, &opts).unwrap());
        assert_eq!((w, h), (130, 130));
        assert_eq!(opts.views[3].name, "az 45 el 30");
        assert!(parse_views("sideways").is_err());
        assert!(
            render_png(
                &ed,
                &RenderOptions {
                    focus: Some(99),
                    ..RenderOptions::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn renders_animated_poses() {
        let mut ed = scene();
        let batch: CommandBatch = serde_json::from_value(serde_json::json!({"commands": [
            {"op": "set_keyframe", "id": "Red", "property": "translation", "frame": 1, "value": [-0.8, 0.5, 0]},
            {"op": "set_keyframe", "id": "Red", "property": "translation", "frame": 10, "value": [0.8, 0.5, 2.0]},
            {"op": "set_keyframe", "id": "Blue", "property": "color", "frame": 10, "value": "#20c040"}
        ]}))
        .unwrap();
        ed.apply(&batch).unwrap();
        let top = |frame| {
            let opts = RenderOptions {
                views: parse_views("top").unwrap(),
                size: 96,
                focus: None,
                frame,
                samples: None,
            };
            decode(&render_png(&ed, &opts).unwrap()).2
        };
        assert_ne!(
            top(Some(1.0)),
            top(Some(10.0)),
            "the pose changes between frames"
        );
        assert_eq!(
            top(Some(10.0)),
            top(Some(50.0)),
            "poses hold after the last key"
        );
    }

    #[test]
    fn glass_shows_what_is_behind_and_neon_glows() {
        let mut ed = Editor::new();
        ed.apply(
            &serde_json::from_value::<crate::engine::CommandBatch>(serde_json::json!({"commands": [
                {"op": "add", "name": "Back", "primitive": {"kind": "cube"}, "translation": [0, 0.5, -1.5], "scale": [3, 1, 0.2], "color": "#c83232"},
                {"op": "add", "name": "Pane", "primitive": {"kind": "cube"}, "translation": [0, 0.5, 0], "scale": [1, 1, 0.1], "preset": "glass"},
                {"op": "add", "name": "Lamp", "primitive": {"kind": "sphere"}, "translation": [2.5, 0.5, 0], "scale": [0.3, 0.3, 0.3], "preset": "neon", "color": "#20ff40"}
            ]})).unwrap(),
        )
        .unwrap();
        let opts = |focus| RenderOptions {
            views: vec![View::preset("front").unwrap()],
            size: 128,
            focus,
            frame: None,
            samples: None,
        };
        let (_, _, px) = decode(&render_png(&ed, &opts(Some(2))).unwrap());
        let at = |x: usize, y: usize| {
            let i = (y * 128 + x) * 3;
            [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
        };
        let c = at(64, 64);
        assert!(c[0] > c[1] + 30, "red wall through the glass: {c:?}");

        let (_, _, px) = decode(&render_png(&ed, &opts(Some(3))).unwrap());
        let at = |x: usize, y: usize| {
            let i = (y * 128 + x) * 3;
            [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
        };
        let core = at(64, 64);
        assert!(core[1] > 200, "lamp is bright: {core:?}");
        // The halo reaches past the silhouette into the background.
        let lit = px.clone();
        ed.apply(
            &serde_json::from_value::<crate::engine::CommandBatch>(
                serde_json::json!({"commands": [
                    {"op": "material", "id": "Lamp", "emissive_strength": 0}
                ]}),
            )
            .unwrap(),
        )
        .unwrap();
        let (_, _, unlit) = decode(&render_png(&ed, &opts(Some(3))).unwrap());
        let green = |img: &[u8], x: usize| img[(64 * 128 + x) * 3 + 1] as i32;
        let edge = (64..128)
            .find(|&x| (green(&unlit, x) - green(&unlit, 127)).abs() < 4)
            .unwrap();
        let (halo, bg) = (green(&lit, edge + 2), green(&unlit, edge + 2));
        assert!(
            halo > bg + 8,
            "halo {halo} vs unlit {bg} at x = {}",
            edge + 2
        );
    }

    #[test]
    fn focus_frames_one_object_and_empty_scenes_render() {
        let ed = scene();
        let opts = RenderOptions {
            views: parse_views("front").unwrap(),
            size: 64,
            focus: Some(1),
            frame: None,
            samples: None,
        };
        let (_, _, px) = decode(&render_png(&ed, &opts).unwrap());
        let c = &px[(32 * 64 + 32) * 3..(32 * 64 + 32) * 3 + 3];
        assert!(c[0] > c[2] + 30, "focused cube fills the centre: {c:?}");
        render_png(
            &Editor::new(),
            &RenderOptions {
                size: 64,
                ..RenderOptions::default()
            },
        )
        .unwrap();
    }
}
