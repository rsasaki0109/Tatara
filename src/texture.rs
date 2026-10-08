//! Procedural textures. A pattern is a tileable 2D function that mixes a
//! material's `color` with its texture's `color2`; meshes need no UV
//! unwrapping because every face is box-projected along its dominant axis
//! (in object space, in metres, divided by the texture's `scale`). The
//! viewport, the agent renderer and glTF export all use this module, so a
//! pattern looks the same everywhere: export bakes it into a PNG.

use std::io::Cursor;

use glam::DVec3;
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
    /// Size of one tile of the pattern in metres (0.01-100).
    #[serde(default = "d_scale")]
    pub scale: f64,
}

impl Texture {
    pub fn new(pattern: Pattern, color2: &str, scale: f64) -> Texture {
        Texture {
            pattern,
            color2: color2.into(),
            scale,
        }
    }

    pub fn validate(&self) -> Result<(), EngineError> {
        check_color(&self.color2)?;
        if !(0.01..=100.0).contains(&self.scale) {
            return Err(EngineError::new(
                "texture scale must be between 0.01 and 100",
            ));
        }
        Ok(())
    }
}

/// Box projection: texture coordinates (in metres) of point `p` on a face
/// with normal `n`, both in object space. Each axis-facing side reads the
/// right way up; opposite sides are mirrored so the pattern does not flip.
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
        Pattern::None => 0.0,
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
    let mut pixels = Vec::with_capacity((size * size * 3) as usize);
    for y in 0..size {
        for x in 0..size {
            // PNG rows run top to bottom; v grows upward.
            let (u, v) = (
                (x as f64 + 0.5) / size as f64,
                1.0 - (y as f64 + 0.5) / size as f64,
            );
            pixels
                .extend(color_at(base, t, u, v).map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8));
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
        let png = bake_png("#a0703f", &Texture::new(Pattern::Wood, "#6b4426", 0.5), 32);
        assert_eq!(&png[1..4], b"PNG");
    }
}
