//! Headless software renderer, so agents can look at what they built.
//!
//! It rasterizes the evaluated scene on the CPU, with crease-aware normals, a
//! shadow-mapped key light, a ground grid for scale and outlines for
//! legibility. Several named views are tiled into one labelled PNG. No GPU or
//! browser is needed: the editor process answers `GET /api/render` directly.

use glam::dcamera::rh::{proj::directx, view::look_at_mat4};
use glam::{DMat4, DVec3, DVec4};

use crate::engine::{Editor, EngineError};

const SUPERSAMPLE: usize = 2;
const SHADOW_SIZE: usize = 1024;
const FOV_DEGREES: f64 = 36.0;
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
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            views: vec![View::preset("iso").unwrap()],
            size: 512,
            focus: None,
        }
    }
}

struct Tri {
    p: [DVec3; 3],
    n: [DVec3; 3],
    object: u32,
}

struct Shading {
    color: DVec3,
    roughness: f64,
    metalness: f64,
}

struct Prepared {
    tris: Vec<Tri>,
    materials: Vec<Shading>,
    center: DVec3,
    radius: f64,
    ground_y: f64,
}

fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f64) -> f64 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn hex(c: &str) -> DVec3 {
    let ch =
        |i: usize| srgb_to_linear(u8::from_str_radix(&c[i..i + 2], 16).unwrap_or(0) as f64 / 255.0);
    DVec3::new(ch(1), ch(3), ch(5))
}

/// ACES filmic approximation (Narkowicz), applied per channel.
fn tonemap(c: DVec3) -> DVec3 {
    let f = |x: f64| ((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)).clamp(0.0, 1.0);
    DVec3::new(f(c.x), f(c.y), f(c.z))
}

fn prepare(ed: &Editor, focus: Option<u64>) -> Result<Prepared, EngineError> {
    let mut tris = Vec::new();
    let mut materials = Vec::new();
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    let (mut flo, mut fhi) = (lo, hi);
    for o in &ed.scene().objects {
        let m = o.transform.matrix();
        let normal_m = m.inverse().transpose();
        let shaded = crate::gltf::shade(ed.evaluated(o));
        let index = materials.len() as u32;
        materials.push(Shading {
            color: hex(&o.material.color),
            roughness: o.material.roughness,
            metalness: o.material.metalness,
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
            tris.push(Tri {
                p: [world[a], world[b], world[c]],
                n: [normals[a], normals[b], normals[c]],
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

/// Render one square tile, returning sRGB bytes.
fn render_tile(prep: &Prepared, shadow: &ShadowMap, view: &View, size: usize) -> Vec<[u8; 3]> {
    let n = size * SUPERSAMPLE;
    let fov = FOV_DEGREES.to_radians();
    let distance = prep.radius * 1.12 / (fov / 2.0).sin();
    let (az, el) = (
        view.azimuth.to_radians(),
        view.elevation.clamp(-89.5, 89.5).to_radians(),
    );
    let dir = DVec3::new(el.cos() * az.sin(), el.sin(), el.cos() * az.cos());
    let eye = prep.center + dir * distance;
    let view_m = look_at_mat4(eye, prep.center, DVec3::Y);
    let near = (distance - prep.radius * 3.0).max(0.01);
    let proj = directx::perspective(fov, 1.0, near, distance + prep.radius * 20.0);
    let vp = proj * view_m;

    let mut depth = vec![f32::INFINITY; n * n];
    let mut id = vec![EMPTY; n * n];
    let mut normal = vec![DVec3::ZERO; n * n];
    let mut world = vec![DVec3::ZERO; n * n];

    let g = prep.radius * 8.0 + 4.0;
    let gy = prep.ground_y;
    let c = prep.center;
    let ground = [
        DVec3::new(c.x - g, gy, c.z - g),
        DVec3::new(c.x + g, gy, c.z - g),
        DVec3::new(c.x + g, gy, c.z + g),
        DVec3::new(c.x - g, gy, c.z + g),
    ];
    let mut draw = |p: [DVec3; 3], nrm: [DVec3; 3], object: u32| {
        let clip = p.map(|q| vp * q.extend(1.0));
        raster(clip, n, n, |x, y, z, b| {
            let i = y * n + x;
            if (z as f32) < depth[i] {
                depth[i] = z as f32;
                id[i] = object;
                normal[i] = (nrm[0] * b[0] + nrm[1] * b[1] + nrm[2] * b[2]).normalize_or_zero();
                world[i] = p[0] * b[0] + p[1] * b[1] + p[2] * b[2];
            }
        });
    };
    if view.elevation > -5.0 {
        draw([ground[0], ground[2], ground[1]], [DVec3::Y; 3], GROUND);
        draw([ground[0], ground[3], ground[2]], [DVec3::Y; 3], GROUND);
    }
    for t in &prep.tris {
        draw(t.p, t.n, t.object);
    }

    let bg_top = hex("#4a4c53");
    let bg_bottom = hex("#1d1e22");
    let ground_col = hex("#34363c");
    let rim = DVec3::new(-5.0, 4.0, -6.0).normalize();
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
                    let p = world[i];
                    let v = (eye - p).normalize();
                    let mut nn = normal[i];
                    if nn.dot(v) < 0.0 {
                        nn = -nn;
                    }
                    let l = shadow.light;
                    let lit = shadow.lit(p, nn);
                    let diffuse_col = m.color * (1.0 - m.metalness * 0.85);
                    let hemi = DVec3::new(0.30, 0.31, 0.34)
                        .lerp(DVec3::new(0.62, 0.64, 0.70), 0.5 + 0.5 * nn.y);
                    let key = nn.dot(l).max(0.0) * lit * 2.0;
                    let fill = nn.dot(rim).max(0.0) * 0.55;
                    let h = (l + v).normalize();
                    let shininess = (2.0 / m.roughness.max(0.05).powi(4) - 2.0).clamp(2.0, 2048.0);
                    let spec_strength = (1.0 - m.roughness).powi(2) * 0.9 + 0.04;
                    let spec_col = DVec3::splat(1.0).lerp(m.color, m.metalness);
                    let spec = nn.dot(h).max(0.0).powf(shininess)
                        * spec_strength
                        * lit
                        * 2.3
                        * (shininess + 8.0)
                        / 64.0;
                    let fresnel = (1.0 - nn.dot(v).max(0.0)).powi(5) * 0.25;
                    diffuse_col * (hemi * 0.9 + DVec3::splat(key + fill))
                        + spec_col * (spec + fresnel)
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

fn draw_text(pixels: &mut [[u8; 3]], size: usize, text: &str, x0: usize, y0: usize, scale: usize) {
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
    if opts.views.is_empty() || opts.views.len() > 6 {
        return Err(EngineError::new("request between 1 and 6 views"));
    }
    if !(64..=1024).contains(&opts.size) {
        return Err(EngineError::new("size must be between 64 and 1024"));
    }
    let prep = prepare(ed, opts.focus)?;
    let shadow = ShadowMap::build(&prep);
    let size = opts.size as usize;
    let cols = if opts.views.len() == 1 { 1 } else { 2 };
    let rows = opts.views.len().div_ceil(cols);
    let gap = 2;
    let (w, h) = (
        cols * size + (cols - 1) * gap,
        rows * size + (rows - 1) * gap,
    );
    let mut image = vec![12u8; w * h * 3];
    for (k, view) in opts.views.iter().enumerate() {
        let tile = render_tile(&prep, &shadow, view, size);
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
    fn tiles_views_into_a_grid() {
        let ed = scene();
        let opts = RenderOptions {
            views: parse_views("front, right, top, 45:30").unwrap(),
            size: 64,
            focus: None,
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
    fn focus_frames_one_object_and_empty_scenes_render() {
        let ed = scene();
        let opts = RenderOptions {
            views: parse_views("front").unwrap(),
            size: 64,
            focus: Some(1),
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
