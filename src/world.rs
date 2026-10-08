//! The world: what lights the scene from afar and what renders show behind
//! it. The default is the studio (a key light, a rim light and a soft
//! dome, as in the viewport). Other skies are environment maps: built-in
//! presets (daylight, sunset, overcast, night) baked from analytic skies,
//! or a scene image (an equirectangular panorama; Radiance `.hdr` files
//! keep real light levels, so a sun in them casts sharp shadows).
//!
//! An environment map is sampled in proportion to its brightness, so the
//! path tracer finds small bright suns quickly, and its sun (if it has a
//! clear one) can be split off as a shadow-casting light for the viewport.

use std::sync::{Arc, Mutex};

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::{EngineError, Scene};

/// The built-in skies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Sky {
    /// Key and rim lights with a soft dome, like the viewport.
    #[default]
    Studio,
    /// Blue sky with a high sun.
    Daylight,
    /// A low orange sun under a warm sky.
    Sunset,
    /// Soft, even light from a grey sky.
    Overcast,
    /// A dark blue sky lit by the moon.
    Night,
}

/// The scene's world settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct World {
    #[serde(default)]
    pub sky: Sky,
    /// A scene image to light the scene with instead of `sky`: an
    /// equirectangular panorama (2:1), ideally a Radiance `.hdr` file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// How bright the world is (0-16; 1 as authored).
    #[serde(default = "one")]
    pub strength: f64,
    /// Turn of the world around the up axis, in degrees.
    #[serde(default)]
    pub rotation: f64,
    /// Show the world behind the scene in renders instead of the studio
    /// backdrop (the floor grid then goes too).
    #[serde(default)]
    pub background: bool,
}

fn one() -> f64 {
    1.0
}

impl Default for World {
    fn default() -> Self {
        Self {
            sky: Sky::Studio,
            image: None,
            strength: 1.0,
            rotation: 0.0,
            background: false,
        }
    }
}

/// Most a world may be brightened.
pub const MAX_STRENGTH: f64 = 16.0;

impl World {
    pub fn is_default(&self) -> bool {
        *self == World::default()
    }

    /// The studio lights (not an environment map).
    pub fn is_studio(&self) -> bool {
        self.sky == Sky::Studio && self.image.is_none()
    }

    pub fn validate(&self, scene: &Scene) -> Result<(), EngineError> {
        if !(0.0..=MAX_STRENGTH).contains(&self.strength) {
            return Err(EngineError::new(format!(
                "world strength must be 0-{MAX_STRENGTH}"
            )));
        }
        if !self.rotation.is_finite() {
            return Err(EngineError::new("world rotation must be a number"));
        }
        if let Some(name) = &self.image
            && !scene.images.contains_key(name)
        {
            return Err(EngineError::new(format!("no image named {name:?}")));
        }
        Ok(())
    }
}

/// An equirectangular environment in linear light, ready to be looked up
/// and importance sampled. Row 0 is straight up; u = 0.5 looks down +X,
/// matching three.js's equirectangular mapping.
pub struct EnvMap {
    pub width: usize,
    pub height: usize,
    pub texels: Vec<[f32; 3]>,
    /// Cumulative weight of each row (the last is 1).
    rows: Vec<f64>,
    /// Cumulative weight within each row (each row's last is 1).
    cols: Vec<f64>,
    /// Each texel's share of the total weight.
    share: Vec<f64>,
}

/// Largest environment kept for lighting: big panoramas are box-filtered
/// down to this width (the light they give barely changes).
pub const MAX_ENV_WIDTH: usize = 1024;

const PI: f64 = std::f64::consts::PI;

fn lum(c: [f32; 3]) -> f64 {
    0.2126 * c[0] as f64 + 0.7152 * c[1] as f64 + 0.0722 * c[2] as f64
}

impl EnvMap {
    pub fn new(width: usize, height: usize, texels: Vec<[f32; 3]>) -> Self {
        assert_eq!(texels.len(), width * height);
        // Weight by brightness and by the solid angle a texel covers, with
        // a floor so every direction can still be sampled.
        let mean = texels.iter().map(|&c| lum(c)).sum::<f64>() / texels.len().max(1) as f64;
        let floor = (mean * 0.02).max(1e-6);
        let mut cols = vec![0.0; width * height];
        let mut row_sums = vec![0.0; height];
        let mut share = vec![0.0; width * height];
        for y in 0..height {
            let cos = Self::elevation(y as f64 + 0.5, height).cos().max(0.0);
            let mut acc = 0.0;
            for x in 0..width {
                let w = (lum(texels[y * width + x]).max(0.0) + floor) * cos;
                share[y * width + x] = w;
                acc += w;
                cols[y * width + x] = acc;
            }
            for c in &mut cols[y * width..(y + 1) * width] {
                *c = if acc > 0.0 { *c / acc } else { 1.0 };
            }
            row_sums[y] = acc;
        }
        let total: f64 = row_sums.iter().sum::<f64>().max(1e-300);
        let mut rows = Vec::with_capacity(height);
        let mut acc = 0.0;
        for s in &row_sums {
            acc += s / total;
            rows.push(acc);
        }
        if let Some(last) = rows.last_mut() {
            *last = 1.0;
        }
        for s in &mut share {
            *s /= total;
        }
        Self {
            width,
            height,
            texels,
            rows,
            cols,
            share,
        }
    }

    /// Elevation (radians, up positive) at row coordinate `y`.
    fn elevation(y: f64, height: usize) -> f64 {
        (0.5 - y / height as f64) * PI
    }

    /// The direction through texture coordinates (`x`, `y`) in texels.
    fn direction(&self, x: f64, y: f64) -> DVec3 {
        let el = Self::elevation(y, self.height);
        let az = (x / self.width as f64 - 0.5) * 2.0 * PI;
        DVec3::new(el.cos() * az.cos(), el.sin(), el.cos() * az.sin())
    }

    /// Texture coordinates (in texels) of direction `d`.
    fn coords(&self, d: DVec3) -> (f64, f64) {
        let u = d.z.atan2(d.x) / (2.0 * PI) + 0.5;
        let v = d.y.clamp(-1.0, 1.0).asin() / PI + 0.5;
        (u * self.width as f64, (1.0 - v) * self.height as f64)
    }

    fn texel(&self, d: DVec3) -> usize {
        let (x, y) = self.coords(d);
        let x = (x.floor() as i64).rem_euclid(self.width as i64) as usize;
        let y = (y.floor() as i64).clamp(0, self.height as i64 - 1) as usize;
        y * self.width + x
    }

    /// Radiance from direction `d` (bilinear).
    pub fn radiance(&self, d: DVec3) -> DVec3 {
        let (x, y) = self.coords(d);
        let (x, y) = (x - 0.5, y - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |i: f64, j: f64| {
            let i = (i as i64).rem_euclid(self.width as i64) as usize;
            let j = (j as i64).clamp(0, self.height as i64 - 1) as usize;
            let c = self.texels[j * self.width + i];
            DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64)
        };
        let top = at(x0, y0).lerp(at(x0 + 1.0, y0), fx);
        let bottom = at(x0, y0 + 1.0).lerp(at(x0 + 1.0, y0 + 1.0), fx);
        top.lerp(bottom, fy)
    }

    /// A direction drawn in proportion to brightness, and its density
    /// over solid angle.
    pub fn sample(&self, u1: f64, u2: f64, u3: f64, u4: f64) -> (DVec3, f64) {
        let y = self.rows.partition_point(|&c| c < u1).min(self.height - 1);
        let row = &self.cols[y * self.width..(y + 1) * self.width];
        let x = row.partition_point(|&c| c < u2).min(self.width - 1);
        let d = self.direction(x as f64 + u3, y as f64 + u4);
        (d, self.pdf(d))
    }

    /// Density of `sample` drawing direction `d`, over solid angle.
    pub fn pdf(&self, d: DVec3) -> f64 {
        let cos = (1.0 - d.y * d.y).max(0.0).sqrt();
        if cos < 1e-9 {
            return 0.0;
        }
        let texel = (2.0 * PI / self.width as f64) * (PI / self.height as f64);
        self.share[self.texel(d)] / (texel * cos)
    }

    /// The light arriving from all around, per unit area facing each way:
    /// the mean radiance (for flat ambient light).
    pub fn mean(&self) -> DVec3 {
        let mut sum = DVec3::ZERO;
        let mut weight = 0.0;
        for y in 0..self.height {
            let cos = Self::elevation(y as f64 + 0.5, self.height).cos();
            for x in 0..self.width {
                let c = self.texels[y * self.width + x];
                sum += DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64) * cos;
            }
            weight += cos * self.width as f64;
        }
        sum / weight.max(1e-12)
    }

    /// A clear sun, if one stands out: its direction, the light it gives a
    /// surface facing it (radiance times solid angle) and the map without
    /// it (its texels lowered to the sky around it). The viewport lights
    /// with the rest and casts the sun's shadows with a directional light.
    pub fn sun(&self) -> Option<(DVec3, DVec3, Vec<[f32; 3]>)> {
        let peak = self.texels.iter().map(|&c| lum(c)).fold(0.0, f64::max);
        if peak <= 0.0 {
            return None;
        }
        let texel = (2.0 * PI / self.width as f64) * (PI / self.height as f64);
        let threshold = peak * 0.02;
        let mut sky_peak: f64 = 0.0;
        let (mut power, mut total, mut dir, mut area) = (DVec3::ZERO, 0.0, DVec3::ZERO, 0.0);
        for y in 0..self.height {
            let cos = Self::elevation(y as f64 + 0.5, self.height).cos();
            for x in 0..self.width {
                let c = self.texels[y * self.width + x];
                let l = lum(c);
                let solid = texel * cos;
                total += l * solid;
                if l >= threshold {
                    power += DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64) * solid;
                    dir += self.direction(x as f64 + 0.5, y as f64 + 0.5) * l * solid;
                    area += solid;
                } else {
                    sky_peak = sky_peak.max(l);
                }
            }
        }
        // A sun is small, far brighter than its sky and carries a good
        // share of the light.
        if area > 0.05 || peak < sky_peak * 8.0 || lum_d(power) < total * 0.2 {
            return None;
        }
        let rest = self
            .texels
            .iter()
            .map(|&c| {
                let l = lum(c);
                if l >= threshold {
                    let k = (sky_peak / l) as f32;
                    c.map(|v| v * k)
                } else {
                    c
                }
            })
            .collect();
        Some((dir.normalize_or(DVec3::Y), power, rest))
    }

    /// A copy no wider than `width` (box filtered).
    pub fn shrink(&self, width: usize) -> Arc<EnvMap> {
        let k = self.width.div_ceil(width.max(1)).max(1);
        let (w, h) = ((self.width / k).max(1), (self.height / k).max(1));
        let mut out = vec![[0f32; 3]; w * h];
        for (y, row) in out.chunks_mut(w).enumerate() {
            for (x, px) in row.iter_mut().enumerate() {
                let mut acc = [0f32; 3];
                let mut n = 0.0;
                for sy in y * k..((y + 1) * k).min(self.height) {
                    for sx in x * k..((x + 1) * k).min(self.width) {
                        let c = self.texels[sy * self.width + sx];
                        for i in 0..3 {
                            acc[i] += c[i];
                        }
                        n += 1.0;
                    }
                }
                *px = acc.map(|v| v / n);
            }
        }
        Arc::new(EnvMap::new(w, h, out))
    }
}

fn lum_d(c: DVec3) -> f64 {
    c.dot(DVec3::new(0.2126, 0.7152, 0.0722))
}

/// Radiance of a built-in sky in direction `d`.
pub fn sky_radiance(sky: Sky, d: DVec3) -> DVec3 {
    let up = d.y;
    // A smooth ground below the horizon.
    let ground = |c: DVec3| c * (0.6 + 0.4 * (-up).clamp(0.0, 1.0));
    // A disc of angular radius `size` (radians) with a soft edge, and a
    // glow around it.
    let disc = |dir: DVec3, size: f64, glow: f64| {
        let a = d.dot(dir).clamp(-1.0, 1.0).acos();
        let core = ((size * 1.15 - a) / (size * 0.3)).clamp(0.0, 1.0);
        (core, (-a / glow).exp())
    };
    let from = |az: f64, el: f64| {
        let (az, el) = (az.to_radians(), el.to_radians());
        DVec3::new(el.cos() * az.cos(), el.sin(), el.cos() * az.sin())
    };
    match sky {
        Sky::Studio => DVec3::ZERO,
        Sky::Daylight => {
            let sun = from(35.0, 52.0);
            let (core, halo) = disc(sun, 0.0125, 0.12);
            if up < 0.0 {
                return ground(DVec3::new(0.32, 0.3, 0.27));
            }
            let t = up.powf(0.45);
            let sky = DVec3::new(0.78, 0.86, 0.98).lerp(DVec3::new(0.18, 0.36, 0.78), t);
            sky * 0.85 + DVec3::new(1.0, 0.95, 0.85) * (halo * 0.9 + core * 11000.0)
        }
        Sky::Sunset => {
            let sun = from(-60.0, 9.0);
            let (core, halo) = disc(sun, 0.016, 0.35);
            if up < 0.0 {
                return ground(DVec3::new(0.12, 0.08, 0.07));
            }
            let t = up.powf(0.5);
            let sky = DVec3::new(1.15, 0.52, 0.22).lerp(DVec3::new(0.16, 0.18, 0.36), t);
            sky * 0.7 + DVec3::new(1.0, 0.55, 0.22) * (halo * 1.6 + core * 6500.0)
        }
        Sky::Overcast => {
            if up < 0.0 {
                return ground(DVec3::new(0.26, 0.26, 0.26));
            }
            // Brighter overhead (the CIE overcast sky).
            DVec3::new(0.86, 0.88, 0.92) * (1.0 + 2.0 * up) / 3.0 * 1.6
        }
        Sky::Night => {
            let moon = from(120.0, 38.0);
            let (core, halo) = disc(moon, 0.02, 0.08);
            if up < 0.0 {
                return ground(DVec3::new(0.02, 0.022, 0.03));
            }
            let t = up.powf(0.5);
            let sky = DVec3::new(0.1, 0.12, 0.22).lerp(DVec3::new(0.02, 0.028, 0.07), t);
            sky + DVec3::new(0.75, 0.82, 1.0) * (halo * 0.15 + core * 1100.0)
        }
    }
}

/// The studio dome as a map, for showing behind the scene.
pub fn studio_map(width: usize, dome: impl Fn(DVec3) -> DVec3) -> EnvMap {
    bake(width, dome)
}

fn bake(width: usize, f: impl Fn(DVec3) -> DVec3) -> EnvMap {
    let height = width / 2;
    let probe = EnvMap {
        width,
        height,
        texels: Vec::new(),
        rows: Vec::new(),
        cols: Vec::new(),
        share: Vec::new(),
    };
    let mut texels = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            // 3x3 samples per texel, so small suns keep their power.
            let mut c = DVec3::ZERO;
            for sy in 0..3 {
                for sx in 0..3 {
                    let d = probe.direction(
                        x as f64 + (sx as f64 + 0.5) / 3.0,
                        y as f64 + (sy as f64 + 0.5) / 3.0,
                    );
                    c += f(d);
                }
            }
            let c = c / 9.0;
            texels.push([c.x as f32, c.y as f32, c.z as f32]);
        }
    }
    EnvMap::new(width, height, texels)
}

/// Width of the baked built-in skies.
const SKY_WIDTH: usize = 1024;

/// What an environment map was made from; the last few are kept.
#[derive(Clone, PartialEq)]
enum Source {
    Sky(Sky),
    Image(Arc<[u8]>),
}

type MapCache = Mutex<Vec<(Source, Arc<EnvMap>)>>;
static MAPS: MapCache = Mutex::new(Vec::new());

/// The environment map for `world` (None for the studio). Maps are built
/// once and shared.
pub fn env_map(world: &World, scene: &Scene) -> Result<Option<Arc<EnvMap>>, EngineError> {
    let source = match &world.image {
        Some(name) => {
            let image = scene
                .images
                .get(name)
                .ok_or_else(|| EngineError::new(format!("no image named {name:?}")))?;
            Source::Image(image.data.clone())
        }
        None if world.sky == Sky::Studio => return Ok(None),
        None => Source::Sky(world.sky),
    };
    {
        let cache = MAPS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, map)) = cache.iter().find(|(s, _)| *s == source) {
            return Ok(Some(map.clone()));
        }
    }
    let map = match (&source, &world.image) {
        (Source::Sky(sky), _) => Arc::new(bake(SKY_WIDTH, |d| sky_radiance(*sky, d))),
        (Source::Image(_), Some(name)) => {
            let px = scene.images[name].linear()?;
            let map = EnvMap::new(px.width as usize, px.height as usize, px.rgb);
            if map.width > MAX_ENV_WIDTH {
                map.shrink(MAX_ENV_WIDTH)
            } else {
                Arc::new(map)
            }
        }
        (Source::Image(_), None) => unreachable!("images come from world.image"),
    };
    let mut cache = MAPS.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(s, _)| *s != source);
    cache.push((source, map.clone()));
    if cache.len() > 3 {
        cache.remove(0);
    }
    Ok(Some(map))
}

/// Turn `d` by `degrees` around the up axis.
pub fn turn(d: DVec3, degrees: f64) -> DVec3 {
    let (s, c) = degrees.to_radians().sin_cos();
    DVec3::new(c * d.x - s * d.z, d.y, s * d.x + c * d.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(seed: &mut u64) -> f64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 11) as f64 / (1u64 << 53) as f64
    }

    #[test]
    fn directions_and_coordinates_agree() {
        let map = bake(64, |d| DVec3::splat(d.y.max(0.0)));
        for d in [
            DVec3::new(1.0, 0.2, 0.3).normalize(),
            DVec3::new(-0.4, -0.7, 0.2).normalize(),
            DVec3::new(0.1, 0.9, -0.8).normalize(),
        ] {
            let (x, y) = map.coords(d);
            assert!(map.direction(x, y).distance(d) < 1e-9);
        }
        // Three.js's mapping: the middle of the map looks down +X.
        assert!(map.direction(32.0, 16.0).distance(DVec3::X) < 1e-9);
        assert!(map.radiance(DVec3::Y).x > 0.95 && map.radiance(DVec3::NEG_Y).x < 1e-9);
    }

    #[test]
    fn sampling_follows_brightness_and_its_density_integrates_to_one() {
        let map = bake(128, |d| sky_radiance(Sky::Sunset, d));
        let mut seed = 0x9e37_79b9_7f4a_7c15;
        // Summed over a grid finer than the texels, the density covers
        // the sphere once.
        let k = 4;
        let (w, h) = (map.width * k, map.height * k);
        let mut integral = 0.0;
        for y in 0..h {
            for x in 0..w {
                let d = map.direction((x as f64 + 0.5) / k as f64, (y as f64 + 0.5) / k as f64);
                let solid = (2.0 * PI / w as f64) * (PI / h as f64) * (1.0 - d.y * d.y).sqrt();
                integral += map.pdf(d) * solid;
            }
        }
        assert!(
            (integral - 1.0).abs() < 0.03,
            "pdf integrates to {integral}"
        );
        // Most samples head for the low sun.
        let sun = {
            let (az, el) = ((-60f64).to_radians(), 9f64.to_radians());
            DVec3::new(el.cos() * az.cos(), el.sin(), el.cos() * az.sin())
        };
        let near = (0..2000)
            .filter(|_| {
                let (d, pdf) = map.sample(
                    rng(&mut seed),
                    rng(&mut seed),
                    rng(&mut seed),
                    rng(&mut seed),
                );
                assert!(pdf > 0.0);
                d.dot(sun) > 0.995
            })
            .count();
        assert!(near > 600, "{near} of 2000 samples near the sun");
    }

    #[test]
    fn suns_are_found_and_split_off() {
        for (sky, has_sun) in [
            (Sky::Daylight, true),
            (Sky::Sunset, true),
            (Sky::Overcast, false),
        ] {
            let map = bake(256, |d| sky_radiance(sky, d));
            match map.sun() {
                Some((dir, power, rest)) => {
                    assert!(has_sun, "{sky:?} has no sun");
                    assert!(dir.y > 0.0 && lum_d(power) > 0.5, "{sky:?}: {dir} {power}");
                    let peak = rest.iter().map(|&c| lum(c)).fold(0.0, f64::max);
                    assert!(peak < 100.0, "{sky:?} keeps a peak of {peak}");
                }
                None => assert!(!has_sun, "{sky:?} lost its sun"),
            }
        }
        let daylight = bake(256, |d| sky_radiance(Sky::Daylight, d));
        let (dir, ..) = daylight.sun().unwrap();
        let want = {
            let (az, el) = (35f64.to_radians(), 52f64.to_radians());
            DVec3::new(el.cos() * az.cos(), el.sin(), el.cos() * az.sin())
        };
        assert!(dir.dot(want) > 0.999);
    }

    #[test]
    fn the_world_command_sets_and_checks_the_world() {
        use crate::engine::{CommandBatch, Editor};
        let mut ed = Editor::new();
        let apply = |ed: &mut Editor, commands: serde_json::Value| {
            let batch: CommandBatch =
                serde_json::from_value(serde_json::json!({ "commands": commands })).unwrap();
            ed.apply(&batch).map(|_| ())
        };
        assert!(ed.scene().world.is_default());
        let hdr = crate::image::tests::hdr_file(8, 4, true, |x, _| [x as f32, 1.0, 2.0]);
        let data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &hdr);
        apply(
            &mut ed,
            serde_json::json!([
                {"op": "add_image", "name": "Sky", "data": data},
                {"op": "world", "image": "Sky", "strength": 2, "rotation": -90, "background": true}
            ]),
        )
        .unwrap();
        let w = &ed.scene().world;
        assert_eq!(
            (w.image.as_deref(), w.strength, w.rotation, w.background),
            (Some("Sky"), 2.0, 270.0, true)
        );
        assert!(!w.is_studio());
        // The image lights the world, so it stays.
        assert!(
            apply(
                &mut ed,
                serde_json::json!([{"op": "delete_image", "name": "Sky"}])
            )
            .is_err()
        );
        let map = env_map(&ed.scene().world, ed.scene()).unwrap().unwrap();
        assert_eq!((map.width, map.height), (8, 4));
        // A sky drops the image; "" goes back to the sky too.
        apply(
            &mut ed,
            serde_json::json!([{"op": "world", "sky": "night"}]),
        )
        .unwrap();
        assert_eq!(
            (ed.scene().world.sky, ed.scene().world.image.as_deref()),
            (Sky::Night, None)
        );
        apply(
            &mut ed,
            serde_json::json!([{"op": "world", "image": "Sky"}, {"op": "world", "image": ""}]),
        )
        .unwrap();
        assert!(ed.scene().world.image.is_none());
        apply(
            &mut ed,
            serde_json::json!([{"op": "delete_image", "name": "Sky"}]),
        )
        .unwrap();
        // Bad values change nothing.
        for bad in [
            serde_json::json!({"op": "world", "strength": 17}),
            serde_json::json!({"op": "world", "image": "Missing"}),
            serde_json::json!({"op": "world", "sky": "noon"}),
        ] {
            let before = ed.scene().world.clone();
            let batch: Result<CommandBatch, _> =
                serde_json::from_value(serde_json::json!({ "commands": [bad] }));
            if let Ok(batch) = batch {
                assert!(ed.apply(&batch).is_err());
            }
            assert_eq!(ed.scene().world, before);
        }
        // Saved scenes keep the world; a missing image is refused on load.
        let mut saved = serde_json::to_value(ed.scene()).unwrap();
        assert_eq!(saved["world"]["sky"], "night");
        saved["world"]["image"] = "Gone".into();
        assert!(ed.load(serde_json::from_value(saved).unwrap()).is_err());
    }

    #[test]
    fn shrinking_keeps_the_light() {
        let map = bake(256, |d| sky_radiance(Sky::Daylight, d));
        let small = map.shrink(64);
        assert_eq!((small.width, small.height), (64, 32));
        let (a, b) = (lum_d(map.mean()), lum_d(small.mean()));
        assert!((a - b).abs() < a * 0.05, "{a} vs {b}");
        assert!(turn(DVec3::X, 90.0).distance(DVec3::Z) < 1e-12);
    }
}
