//! Textures. A pattern is a tileable 2D function that mixes a material's
//! `color` with its texture's `color2`, or an image from the scene that the
//! colour tints. Meshes need no UV unwrapping: every face is box-projected
//! along its dominant axis (in object space, in metres, divided by the
//! texture's `scale`), unless the mesh brings its own UVs (glTF import).
//! `relief` turns the pattern into bumps (mortar and grout sink in) and
//! `normal_map` takes the normals from an image instead. The viewport, the
//! agent renderer and glTF export all use this module, so a texture looks
//! the same everywhere: export bakes patterns and relief into PNGs.

use std::io::Cursor;

use glam::DVec3;

use crate::image::Pixels;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::{EngineError, check_color};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    /// No texture (removes one).
    None,
    Checker,
    Stripes,
    /// Square tiles with grout lines in `color2`.
    Tiles,
    /// Running-bond bricks with mortar in `color2`.
    Brick,
    /// Wood grain along the U direction, rings in `color2`.
    Wood,
    /// Marble veins in `color2`.
    Marble,
    /// The scene image named by `image`, tinted by the material colour.
    Image,
}

fn d_color2() -> String {
    "#3b2a22".into()
}
fn d_scale() -> f64 {
    0.5
}

/// A procedural pattern on a material. `color` is the material's own colour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Texture {
    pub pattern: Pattern,
    /// Second colour of the pattern, `#rrggbb`.
    #[serde(default = "d_color2")]
    pub color2: String,
    /// Size of one tile of the pattern in metres (0.01-100). Meshes with
    /// their own UVs use those instead.
    #[serde(default = "d_scale")]
    pub scale: f64,
    /// For pattern `image`: the name of a scene image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Bump depth from the pattern (0-1): grout, mortar and grain sink in.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub relief: f64,
    /// A scene image used as a tangent-space normal map (overrides relief).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_map: Option<String>,
    /// Stretch one tile over each side of the object instead of repeating
    /// every `scale` metres (labels, posters, photos).
    #[serde(default, skip_serializing_if = "is_false")]
    pub fit: bool,
}

fn is_false(x: &bool) -> bool {
    !*x
}

fn is_zero(x: &f64) -> bool {
    *x == 0.0
}

/// Height range of full relief, in tile units.
const RELIEF_DEPTH: f64 = 0.02;

impl Texture {
    pub fn new(pattern: Pattern, color2: &str, scale: f64) -> Texture {
        Texture {
            pattern,
            color2: color2.into(),
            scale,
            image: None,
            relief: 0.0,
            normal_map: None,
            fit: false,
        }
    }

    pub fn with_relief(mut self, relief: f64) -> Texture {
        self.relief = relief;
        self
    }

    pub fn validate(&self) -> Result<(), EngineError> {
        check_color(&self.color2)?;
        if !(0.01..=100.0).contains(&self.scale) {
            return Err(EngineError::new(
                "texture scale must be between 0.01 and 100",
            ));
        }
        if !(0.0..=1.0).contains(&self.relief) {
            return Err(EngineError::new("texture relief must be between 0 and 1"));
        }
        if (self.pattern == Pattern::Image) != self.image.is_some() {
            return Err(EngineError::new(
                "pattern \"image\" needs an `image` (and only it takes one)",
            ));
        }
        Ok(())
    }

    /// Scene images this texture refers to.
    pub fn images(&self) -> impl Iterator<Item = &str> {
        self.image
            .iter()
            .chain(&self.normal_map)
            .map(String::as_str)
    }
}

/// A texture ready to sample: the material colour plus decoded images.
pub struct Look<'a> {
    pub base: &'a str,
    pub texture: &'a Texture,
    pub image: Option<&'a Pixels>,
    pub normal_map: Option<&'a Pixels>,
}

impl Look<'_> {
    /// sRGB albedo at (u, v) in tile units.
    pub fn color(&self, u: f64, v: f64) -> [f64; 3] {
        match (self.texture.pattern, self.image) {
            (Pattern::Image, Some(px)) => {
                let (tint, c) = (rgb(self.base), px.sample(u, v));
                [0, 1, 2].map(|i| tint[i] * c[i])
            }
            _ => color_at(self.base, self.texture, u, v),
        }
    }

    fn height(&self, u: f64, v: f64) -> f64 {
        match (self.texture.pattern, self.image) {
            (Pattern::Image, Some(px)) => {
                let c = px.sample(u, v);
                0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
            }
            (p, _) => 1.0 - sample(p, u, v),
        }
    }

    /// Whether the surface normal is perturbed at all.
    pub fn bumpy(&self) -> bool {
        self.normal_map.is_some() || self.texture.relief > 0.0
    }

    /// Tangent-space normal at (u, v): x along +u, y along +v, z out.
    /// `step` is the sampling distance in tile units (about one texel).
    pub fn normal(&self, u: f64, v: f64, step: f64) -> DVec3 {
        if let Some(px) = self.normal_map {
            let c = px.sample(u, v);
            return DVec3::new(c[0] * 2.0 - 1.0, c[1] * 2.0 - 1.0, c[2] * 2.0 - 1.0)
                .normalize_or(DVec3::Z);
        }
        if self.texture.relief <= 0.0 {
            return DVec3::Z;
        }
        let depth = self.texture.relief * RELIEF_DEPTH;
        let du = (self.height(u + step, v) - self.height(u - step, v)) / (2.0 * step);
        let dv = (self.height(u, v + step) - self.height(u, v - step)) / (2.0 * step);
        DVec3::new(-du * depth, -dv * depth, 1.0).normalize()
    }
}

/// Which of the six box-projection sides a face normal falls on.
pub fn side(n: DVec3) -> u8 {
    let a = n.abs();
    let (k, c) = if a.x >= a.y && a.x >= a.z {
        (0, n.x)
    } else if a.y >= a.z {
        (1, n.y)
    } else {
        (2, n.z)
    };
    k * 2 + u8::from(c < 0.0)
}

/// Box projection of one object: in metres of its scaled size, or fitted
/// so each side shows exactly one tile.
pub struct BoxProjection {
    scale: DVec3,
    /// Per side: the lowest (u, v) and the extent, for `fit`.
    fit: Option<[[f64; 4]; 6]>,
}

impl BoxProjection {
    pub fn new(vertices: &[[f64; 3]], scale: [f64; 3], fit: bool) -> BoxProjection {
        let scale = DVec3::from_array(scale);
        let fit = fit.then(|| {
            let (lo, hi) = vertices.iter().fold(
                (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)),
                |(lo, hi), v| (lo.min(DVec3::from_array(*v)), hi.max(DVec3::from_array(*v))),
            );
            let (lo, hi) = if lo.x.is_finite() {
                (lo * scale, hi * scale)
            } else {
                (DVec3::ZERO, DVec3::ONE)
            };
            let normals = [
                DVec3::X,
                DVec3::NEG_X,
                DVec3::Y,
                DVec3::NEG_Y,
                DVec3::Z,
                DVec3::NEG_Z,
            ];
            normals.map(|n| {
                let corners = (0..8).map(|k| {
                    DVec3::new(
                        if k & 1 == 0 { lo.x } else { hi.x },
                        if k & 2 == 0 { lo.y } else { hi.y },
                        if k & 4 == 0 { lo.z } else { hi.z },
                    )
                });
                let (mut a, mut b) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
                for c in corners {
                    let uv = box_uv(c, n);
                    for i in 0..2 {
                        a[i] = a[i].min(uv[i]);
                        b[i] = b[i].max(uv[i]);
                    }
                }
                [a[0], a[1], (b[0] - a[0]).max(1e-9), (b[1] - a[1]).max(1e-9)]
            })
        });
        BoxProjection { scale, fit }
    }

    /// The side and coordinates of object-space point `p` on a face with
    /// object-space normal `n`: metres, or tiles when fitted.
    pub fn uv(&self, p: DVec3, n: DVec3) -> (u8, [f64; 2]) {
        let s = self.scale;
        let safe = |x: f64| if x.abs() < 1e-9 { 1e-9 } else { x };
        let n = DVec3::new(n.x / safe(s.x), n.y / safe(s.y), n.z / safe(s.z));
        let side = side(n);
        let uv = box_uv(p * s, n);
        match &self.fit {
            Some(f) => {
                let [u0, v0, du, dv] = f[side as usize];
                (side, [(uv[0] - u0) / du, (uv[1] - v0) / dv])
            }
            None => (side, uv),
        }
    }

    pub fn fitted(&self) -> bool {
        self.fit.is_some()
    }
}

/// Box projection: texture coordinates (in metres) of point `p` on a face
/// with normal `n`. Each axis-facing side reads the right way up; opposite
/// sides are mirrored so the pattern does not flip.
pub fn box_uv(p: DVec3, n: DVec3) -> [f64; 2] {
    let a = n.abs();
    if a.x >= a.y && a.x >= a.z {
        if n.x >= 0.0 { [-p.z, p.y] } else { [p.z, p.y] }
    } else if a.y >= a.z {
        if n.y >= 0.0 { [p.x, -p.z] } else { [p.x, p.z] }
    } else if n.z >= 0.0 {
        [p.x, p.y]
    } else {
        [-p.x, p.y]
    }
}

fn hash(x: i64, y: i64, seed: i64) -> f64 {
    let mut h = (x.wrapping_mul(374_761_393)
        ^ y.wrapping_mul(668_265_263)
        ^ seed.wrapping_mul(2_147_483_647)) as u64;
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    (h & 0xffff) as f64 / 65535.0
}

/// Value noise with period `n` cells in both directions (tileable on [0,1)).
fn noise(u: f64, v: f64, n: i64, seed: i64) -> f64 {
    let (x, y) = (u * n as f64, v * n as f64);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let s = |t: f64| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (s(fx), s(fy));
    let at = |i: i64, j: i64| hash(i.rem_euclid(n), j.rem_euclid(n), seed);
    let (i, j) = (x0 as i64, y0 as i64);
    let a = at(i, j) + (at(i + 1, j) - at(i, j)) * sx;
    let b = at(i, j + 1) + (at(i + 1, j + 1) - at(i, j + 1)) * sx;
    a + (b - a) * sy
}

fn fbm(u: f64, v: f64, base: i64, seed: i64) -> f64 {
    let mut sum = 0.0;
    let mut amp = 0.5;
    let mut n = base;
    for k in 0..4 {
        sum += noise(u, v, n, seed + k) * amp;
        amp *= 0.5;
        n *= 2;
    }
    sum / 0.9375
}

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How much of `color2` the pattern shows at (u, v), in tile units
/// (the function repeats every 1.0 in both directions).
pub fn sample(pattern: Pattern, u: f64, v: f64) -> f64 {
    let (u, v) = (u.rem_euclid(1.0), v.rem_euclid(1.0));
    match pattern {
        Pattern::None | Pattern::Image => 0.0,
        Pattern::Checker => (((u * 2.0).floor() + (v * 2.0).floor()) as i64 % 2) as f64,
        Pattern::Stripes => smoothstep(0.47, 0.53, ((u * 4.0).fract() - 0.5).abs() * 2.0),
        Pattern::Tiles => {
            let (fu, fv) = ((u * 4.0).fract(), (v * 4.0).fract());
            let edge = fu.min(1.0 - fu).min(fv.min(1.0 - fv));
            let grout = 1.0 - smoothstep(0.03, 0.05, edge);
            let tone = hash((u * 4.0) as i64, (v * 4.0) as i64, 7) * 0.12;
            grout.max(tone)
        }
        Pattern::Brick => {
            let row = (v * 4.0).floor();
            let fu = (u * 2.0 + if row as i64 % 2 == 0 { 0.0 } else { 0.5 }).fract();
            let fv = (v * 4.0).fract();
            let edge = (fu.min(1.0 - fu) * 0.5).min(fv.min(1.0 - fv) * 0.25);
            let mortar = 1.0 - smoothstep(0.004, 0.009, edge);
            let col = ((u * 2.0 + if row as i64 % 2 == 0 { 0.0 } else { 0.5 }).floor() as i64)
                .rem_euclid(2);
            let tone = hash(col, row as i64, 3) * 0.18 + fbm(u, v, 8, 11) * 0.1;
            mortar.max(tone)
        }
        Pattern::Wood => {
            // Rings run along U, wobbling with low-frequency noise.
            let warp = fbm(u, v, 2, 5) * 1.6;
            let rings = ((v * 6.0 + warp) * std::f64::consts::TAU).sin() * 0.5 + 0.5;
            let grain = noise(u, v, 64, 9) * 0.25;
            (rings.powf(3.0) * 0.65 + grain).clamp(0.0, 1.0)
        }
        Pattern::Marble => {
            let turb = fbm(u, v, 4, 21) * 5.0;
            let vein = ((u + v) * std::f64::consts::TAU * 2.0 + turb).sin().abs();
            (1.0 - vein).powf(6.0)
        }
    }
}

fn rgb(hex: &str) -> [f64; 3] {
    let ch = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
    [ch(1), ch(3), ch(5)]
}

/// The sRGB colour of a texture at (u, v) tile units.
pub fn color_at(base: &str, t: &Texture, u: f64, v: f64) -> [f64; 3] {
    let (a, b) = (rgb(base), rgb(&t.color2));
    let k = sample(t.pattern, u, v);
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * k)
}

/// One tile of the texture as an sRGB PNG, `size` pixels square.
pub fn bake_png(base: &str, t: &Texture, size: u32) -> Vec<u8> {
    let look = Look {
        base,
        texture: t,
        image: None,
        normal_map: None,
    };
    bake(size, |u, v| look.color(u, v))
}

/// One tile of the texture's normals as a tangent-space normal map PNG
/// (+X along u, +Y along v: the glTF and OpenGL convention).
pub fn bake_normal_png(look: &Look, size: u32) -> Vec<u8> {
    let step = 1.0 / size as f64;
    bake(size, |u, v| {
        let n = look.normal(u, v, step);
        [n.x, n.y, n.z].map(|c| c * 0.5 + 0.5)
    })
}

fn bake(size: u32, at: impl Fn(f64, f64) -> [f64; 3]) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((size * size * 3) as usize);
    for y in 0..size {
        for x in 0..size {
            // PNG rows run top to bottom; v grows upward.
            let (u, v) = (
                (x as f64 + 0.5) / size as f64,
                1.0 - (y as f64 + 0.5) / size as f64,
            );
            pixels.extend(at(u, v).map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8));
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(Cursor::new(&mut out), size, size);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().expect("png header");
        w.write_image_data(&pixels).expect("png data");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_tile_seamlessly_and_stay_in_range() {
        for p in [
            Pattern::Checker,
            Pattern::Stripes,
            Pattern::Tiles,
            Pattern::Brick,
            Pattern::Wood,
            Pattern::Marble,
        ] {
            for k in 0..50 {
                let (u, v) = (k as f64 * 0.0371, k as f64 * 0.0213);
                let s = sample(p, u, v);
                assert!((0.0..=1.0).contains(&s), "{p:?} {s}");
                assert!(
                    (s - sample(p, u + 1.0, v - 2.0)).abs() < 1e-9,
                    "{p:?} repeats"
                );
            }
            // The edges of a tile meet: no visible seam.
            for k in 0..20 {
                let t = k as f64 / 20.0;
                let (a, b) = (sample(p, 1e-7, t), sample(p, 1.0 - 1e-7, t));
                assert!(
                    (a - b).abs() < 0.05 || p == Pattern::Checker || p == Pattern::Stripes,
                    "{p:?} seam at v={t}: {a} vs {b}"
                );
            }
        }
    }

    #[test]
    fn box_projection_reads_each_side_upright() {
        assert_eq!(box_uv(DVec3::new(0.2, 0.3, 0.5), DVec3::Z), [0.2, 0.3]);
        assert_eq!(box_uv(DVec3::new(0.2, 0.3, 0.5), DVec3::Y), [0.2, -0.5]);
        assert_eq!(box_uv(DVec3::new(0.5, 0.3, 0.2), DVec3::X), [-0.2, 0.3]);
        // Scaled objects tile in real metres; fitted ones span each side once.
        let cube = [[-0.5, -0.5, -0.5], [0.5, 0.5, 0.5]];
        let wall = BoxProjection::new(&cube, [4.0, 2.0, 0.1], false);
        assert_eq!(
            wall.uv(DVec3::new(0.5, 0.5, 0.5), DVec3::Z),
            (4, [2.0, 1.0])
        );
        let poster = BoxProjection::new(&cube, [4.0, 2.0, 0.1], true);
        assert_eq!(
            poster.uv(DVec3::new(0.5, 0.5, 0.5), DVec3::Z),
            (4, [1.0, 1.0])
        );
        assert_eq!(
            poster.uv(DVec3::new(-0.5, -0.5, 0.5), DVec3::Z),
            (4, [0.0, 0.0])
        );
        // The back reads mirrored, so its left edge (seen from behind) is u = 0.
        assert_eq!(
            poster.uv(DVec3::new(0.5, -0.5, -0.5), DVec3::NEG_Z),
            (5, [0.0, 0.0])
        );
        let png = bake_png("#a0703f", &Texture::new(Pattern::Wood, "#6b4426", 0.5), 32);
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn relief_sinks_mortar_and_flat_stays_flat() {
        let flat = Texture::new(Pattern::Brick, "#d8d0c4", 0.5);
        let bumpy = flat.clone().with_relief(1.0);
        let look = |t| Look {
            base: "#a4452c",
            texture: t,
            image: None,
            normal_map: None,
        };
        assert_eq!(look(&flat).normal(0.3, 0.37, 0.002), DVec3::Z);
        // Inside a brick the face is nearly flat (a little grain); on the
        // slope down into the mortar joint between rows (v = 0.25) the
        // normal tilts steeply away from the joint.
        let tilt = |v: f64| 1.0 - look(&bumpy).normal(0.3, v, 0.0005).z;
        let inside = (0..50)
            .map(|k| tilt(0.3 + k as f64 * 0.002))
            .fold(0.0, f64::max);
        let (joint, at) = (0..300)
            .map(|k| 0.25 + k as f64 * 0.0001)
            .map(|v| (tilt(v), v))
            .fold((0.0, 0.0), |a, b| if b.0 > a.0 { b } else { a });
        assert!(
            joint > 0.05 && joint > inside * 5.0,
            "joint {joint} vs inside {inside}"
        );
        assert!(
            look(&bumpy).normal(0.3, at, 0.0005).y < 0.0,
            "rising away from the joint"
        );
        let png = bake_normal_png(&look(&bumpy), 16);
        assert_eq!(&png[1..4], b"PNG");
    }
}
