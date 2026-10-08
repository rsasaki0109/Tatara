//! Image assets: PNG, JPEG or Radiance HDR files stored in the scene by
//! name, so a scene file, an undo step and a glTF round trip all carry them.
//! Materials use them as colour textures or normal maps (see `texture.rs`),
//! and the world uses one as its environment (see `world.rs`); HDR files
//! keep real light levels for that. The bytes are kept as uploaded (and
//! shared between undo snapshots); pixels are decoded only where they are
//! sampled.

use std::io::Cursor;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::engine::EngineError;

/// Largest accepted file, in bytes.
pub const MAX_IMAGE_BYTES: usize = 8 << 20;
/// Largest accepted Radiance HDR file, in bytes (environment maps are big).
pub const MAX_HDR_BYTES: usize = 32 << 20;
/// The media type of Radiance HDR (RGBE) files.
pub const HDR_MIME: &str = "image/vnd.radiance";
/// Largest accepted width or height, in pixels.
pub const MAX_IMAGE_SIDE: u32 = 4096;
/// Most images one scene may hold.
pub const MAX_IMAGES: usize = 32;

fn err<T>(message: impl Into<String>) -> Result<T, EngineError> {
    Err(EngineError::new(message))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImageAsset {
    /// `image/png`, `image/jpeg` or `image/vnd.radiance` (HDR).
    pub mime: String,
    pub width: u32,
    pub height: u32,
    /// The file itself, base64 in JSON.
    #[serde(serialize_with = "to_base64", deserialize_with = "from_base64")]
    #[schemars(with = "String")]
    pub data: Arc<[u8]>,
}

fn to_base64<S: Serializer>(data: &Arc<[u8]>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&BASE64.encode(data))
}

fn from_base64<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<[u8]>, D::Error> {
    let text = String::deserialize(d)?;
    decode_base64(&text)
        .map(Arc::from)
        .map_err(|e| serde::de::Error::custom(e.message))
}

/// Base64, optionally as a `data:` URL.
pub fn decode_base64(text: &str) -> Result<Vec<u8>, EngineError> {
    let body = match text.split_once(";base64,") {
        Some((head, body)) if head.starts_with("data:") => body,
        _ => text,
    };
    BASE64
        .decode(body.trim())
        .map_err(|_| EngineError::new("image data must be base64"))
}

impl ImageAsset {
    /// Check a PNG, JPEG or Radiance HDR file and read its size (without
    /// decoding pixels).
    pub fn from_bytes(bytes: Vec<u8>) -> Result<ImageAsset, EngineError> {
        if is_hdr(&bytes) {
            if bytes.len() > MAX_HDR_BYTES {
                return err(format!(
                    "HDR images are limited to {} MB",
                    MAX_HDR_BYTES >> 20
                ));
            }
            let (width, height, _) = hdr_header(&bytes)?;
            if width == 0 || height == 0 || width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
                return err(format!(
                    "images must be 1-{MAX_IMAGE_SIDE} pixels on a side"
                ));
            }
            return Ok(ImageAsset {
                mime: HDR_MIME.into(),
                width,
                height,
                data: bytes.into(),
            });
        }
        if bytes.len() > MAX_IMAGE_BYTES {
            return err(format!(
                "images are limited to {} MB",
                MAX_IMAGE_BYTES >> 20
            ));
        }
        let (mime, width, height) = if bytes.starts_with(b"\x89PNG") {
            let reader = png::Decoder::new(Cursor::new(&bytes))
                .read_info()
                .map_err(|e| EngineError::new(format!("invalid PNG: {e}")))?;
            let info = reader.info();
            ("image/png", info.width, info.height)
        } else if bytes.starts_with(&[0xff, 0xd8]) {
            let mut d = jpeg_decoder::Decoder::new(Cursor::new(&bytes));
            d.read_info()
                .map_err(|e| EngineError::new(format!("invalid JPEG: {e}")))?;
            let info = d.info().expect("read_info succeeded");
            ("image/jpeg", info.width as u32, info.height as u32)
        } else {
            return err("images must be PNG, JPEG or Radiance HDR (.hdr)");
        };
        if width == 0 || height == 0 || width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
            return err(format!(
                "images must be 1-{MAX_IMAGE_SIDE} pixels on a side"
            ));
        }
        Ok(ImageAsset {
            mime: mime.into(),
            width,
            height,
            data: bytes.into(),
        })
    }

    /// Re-check a stored asset (scene files are untrusted).
    pub fn validate(&self) -> Result<(), EngineError> {
        let checked = ImageAsset::from_bytes(self.data.to_vec())?;
        if (checked.mime.as_str(), checked.width, checked.height)
            != (self.mime.as_str(), self.width, self.height)
        {
            return err("image size or type does not match its data");
        }
        Ok(())
    }

    /// A short fingerprint of the bytes, for cache-busting URLs.
    pub fn hash(&self) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in self.data.iter() {
            h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
        format!("{h:016x}")
    }

    pub fn is_hdr(&self) -> bool {
        self.mime == HDR_MIME
    }

    /// Linear light: HDR files as stored, others from sRGB.
    pub fn linear(&self) -> Result<Pixels, EngineError> {
        if self.is_hdr() {
            let (width, height, rgb) = decode_hdr(&self.data)?;
            return Ok(Pixels { width, height, rgb });
        }
        let mut px = self.decode()?;
        for c in &mut px.rgb {
            *c = c.map(|v| crate::render::srgb_to_linear(v as f64) as f32);
        }
        Ok(px)
    }

    /// The file as a browser or glTF viewer can show it: PNG and JPEG as
    /// they are, HDR as a PNG of its clamped colours.
    pub fn portable(&self) -> Result<(&'static str, Vec<u8>), EngineError> {
        match self.mime.as_str() {
            "image/png" => Ok(("image/png", self.data.to_vec())),
            "image/jpeg" => Ok(("image/jpeg", self.data.to_vec())),
            _ => {
                let px = self.decode()?;
                let rgb: Vec<u8> = px
                    .rgb
                    .iter()
                    .flat_map(|c| c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8))
                    .collect();
                let mut out = Vec::new();
                let mut enc = png::Encoder::new(Cursor::new(&mut out), px.width, px.height);
                enc.set_color(png::ColorType::Rgb);
                enc.write_header()
                    .and_then(|mut w| w.write_image_data(&rgb))
                    .map_err(|e| EngineError::new(format!("cannot encode PNG: {e}")))?;
                Ok(("image/png", out))
            }
        }
    }

    /// Decode to sRGB pixels in 0..1 (HDR light levels are clamped).
    pub fn decode(&self) -> Result<Pixels, EngineError> {
        let bytes: &[u8] = &self.data;
        let fail = |e: String| EngineError::new(format!("cannot decode image: {e}"));
        let (width, height, rgb) = if self.is_hdr() {
            let (w, h, rgb) = decode_hdr(bytes)?;
            let srgb = rgb
                .iter()
                .map(|c| c.map(|v| crate::render::linear_to_srgb(v as f64) as f32))
                .collect();
            (w, h, srgb)
        } else if self.mime == "image/png" {
            let mut decoder = png::Decoder::new(Cursor::new(bytes));
            decoder
                .set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
            let mut reader = decoder.read_info().map_err(|e| fail(e.to_string()))?;
            let mut buf = vec![
                0;
                reader
                    .output_buffer_size()
                    .ok_or_else(|| fail("too large".into()))?
            ];
            let info = reader
                .next_frame(&mut buf)
                .map_err(|e| fail(e.to_string()))?;
            let buf = &buf[..info.buffer_size()];
            let channels = info.color_type.samples();
            let rgb: Vec<[f32; 3]> = buf
                .chunks(channels)
                .map(|c| match channels {
                    1 | 2 => [c[0] as f32 / 255.0; 3],
                    _ => [c[0], c[1], c[2]].map(|x| x as f32 / 255.0),
                })
                .collect();
            (info.width, info.height, rgb)
        } else {
            let mut d = jpeg_decoder::Decoder::new(Cursor::new(bytes));
            let px = d.decode().map_err(|e| fail(e.to_string()))?;
            let info = d.info().expect("decoded");
            let rgb: Vec<[f32; 3]> = match info.pixel_format {
                jpeg_decoder::PixelFormat::L8 => {
                    px.iter().map(|&l| [l as f32 / 255.0; 3]).collect()
                }
                jpeg_decoder::PixelFormat::RGB24 => px
                    .chunks(3)
                    .map(|c| [c[0], c[1], c[2]].map(|x| x as f32 / 255.0))
                    .collect(),
                jpeg_decoder::PixelFormat::CMYK32 => px
                    .chunks(4)
                    .map(|c| {
                        let k = 1.0 - c[3] as f32 / 255.0;
                        [c[0], c[1], c[2]].map(|x| (1.0 - x as f32 / 255.0) * k)
                    })
                    .collect(),
                jpeg_decoder::PixelFormat::L16 => px
                    .chunks(2)
                    .map(|c| [u16::from_be_bytes([c[0], c[1]]) as f32 / 65535.0; 3])
                    .collect(),
            };
            (info.width as u32, info.height as u32, rgb)
        };
        if rgb.len() != (width * height) as usize {
            return Err(fail("unexpected pixel layout".into()));
        }
        Ok(Pixels { width, height, rgb })
    }
}

fn is_hdr(bytes: &[u8]) -> bool {
    bytes.starts_with(b"#?RADIANCE") || bytes.starts_with(b"#?RGBE")
}

/// A Radiance HDR header: width, height and where the pixels start. Only
/// the usual top-down, left-to-right layout (`-Y h +X w`) is accepted.
fn hdr_header(bytes: &[u8]) -> Result<(u32, u32, usize), EngineError> {
    let bad = |m: &str| EngineError::new(format!("invalid HDR: {m}"));
    let mut at = 0;
    let mut line = || -> Result<&[u8], EngineError> {
        let rest = &bytes[at..];
        let end = rest
            .iter()
            .position(|&b| b == b'\n')
            .filter(|&n| n < 4096)
            .ok_or_else(|| bad("header too long"))?;
        at += end + 1;
        Ok(&rest[..end])
    };
    line()?;
    loop {
        let l = line()?;
        if l.is_empty() {
            break;
        }
        if let Some(format) = l.strip_prefix(b"FORMAT=")
            && format != b"32-bit_rle_rgbe"
        {
            return Err(bad("only RGBE pixels are supported"));
        }
    }
    let size = String::from_utf8_lossy(line()?).into_owned();
    let parts: Vec<&str> = size.split_whitespace().collect();
    let (h, w) = match parts.as_slice() {
        ["-Y", h, "+X", w] => (h.parse::<u32>(), w.parse::<u32>()),
        _ => return Err(bad("only the -Y h +X w layout is supported")),
    };
    match (h, w) {
        (Ok(h), Ok(w)) => Ok((w, h, at)),
        _ => Err(bad("bad size")),
    }
}

/// Decode a Radiance HDR file to linear RGB, row 0 at the top.
pub fn decode_hdr(bytes: &[u8]) -> Result<(u32, u32, Vec<[f32; 3]>), EngineError> {
    let bad = || EngineError::new("invalid HDR: truncated pixel data");
    let (w, h, start) = hdr_header(bytes)?;
    let (wu, hu) = (w as usize, h as usize);
    let mut data = &bytes[start..];
    let mut out = Vec::with_capacity(wu * hu);
    let mut row = vec![[0u8; 4]; wu];
    for _ in 0..hu {
        let rle = (8..0x8000).contains(&wu)
            && data.len() >= 4
            && data[0] == 2
            && data[1] == 2
            && ((data[2] as usize) << 8 | data[3] as usize) == wu;
        if rle {
            data = &data[4..];
            for ch in 0..4 {
                let mut x = 0;
                while x < wu {
                    let (&count, rest) = data.split_first().ok_or_else(bad)?;
                    data = rest;
                    if count > 128 {
                        let n = (count - 128) as usize;
                        let (&v, rest) = data.split_first().ok_or_else(bad)?;
                        data = rest;
                        if x + n > wu {
                            return Err(bad());
                        }
                        for px in &mut row[x..x + n] {
                            px[ch] = v;
                        }
                        x += n;
                    } else {
                        let n = count as usize;
                        if n == 0 || x + n > wu || data.len() < n {
                            return Err(bad());
                        }
                        for (px, &v) in row[x..x + n].iter_mut().zip(&data[..n]) {
                            px[ch] = v;
                        }
                        data = &data[n..];
                        x += n;
                    }
                }
            }
        } else {
            if data.len() < wu * 4 {
                return Err(bad());
            }
            let (pixels, _) = data[..wu * 4].as_chunks::<4>();
            row.copy_from_slice(pixels);
            data = &data[wu * 4..];
        }
        out.extend(row.iter().map(|&[r, g, b, e]| {
            if e == 0 {
                [0.0; 3]
            } else {
                let f = 2f32.powi(e as i32 - 136);
                [r as f32 * f, g as f32 * f, b as f32 * f]
            }
        }));
    }
    Ok((w, h, out))
}

/// Decoded pixels, row 0 at the top: sRGB in 0..1, or linear light from
/// `ImageAsset::linear`.
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<[f32; 3]>,
}

impl Pixels {
    /// Bilinear, repeating sample at (u, v); v grows upward like our UVs.
    pub fn sample(&self, u: f64, v: f64) -> [f64; 3] {
        let (w, h) = (self.width as f64, self.height as f64);
        let x = u.rem_euclid(1.0) * w - 0.5;
        let y = (1.0 - v.rem_euclid(1.0)) * h - 0.5;
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |i: f64, j: f64| {
            let i = (i as i64).rem_euclid(self.width as i64) as usize;
            let j = (j as i64).rem_euclid(self.height as i64) as usize;
            self.rgb[j * self.width as usize + i]
        };
        let (a, b, c, d) = (
            at(x0, y0),
            at(x0 + 1.0, y0),
            at(x0, y0 + 1.0),
            at(x0 + 1.0, y0 + 1.0),
        );
        [0, 1, 2].map(|k| {
            let top = a[k] as f64 + (b[k] - a[k]) as f64 * fx;
            let bottom = c[k] as f64 + (d[k] - c[k]) as f64 * fx;
            top + (bottom - top) * fy
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A 2x2 PNG: red, green / blue, white.
    pub(crate) fn tiny_png() -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(Cursor::new(&mut out), 2, 2);
            enc.set_color(png::ColorType::Rgb);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255])
                .unwrap();
        }
        out
    }

    #[test]
    fn reads_checks_and_samples_images() {
        let img = ImageAsset::from_bytes(tiny_png()).unwrap();
        assert_eq!(
            (img.mime.as_str(), img.width, img.height),
            ("image/png", 2, 2)
        );
        img.validate().unwrap();
        let px = img.decode().unwrap();
        // v grows upward: the top row (red, green) is at v near 1.
        assert_eq!(px.sample(0.25, 0.75), [1.0, 0.0, 0.0]);
        assert_eq!(px.sample(0.75, 0.25), [1.0, 1.0, 1.0]);
        assert_eq!(px.sample(1.25, -0.25), [1.0, 0.0, 0.0], "repeats");

        assert!(ImageAsset::from_bytes(b"GIF89a".to_vec()).is_err());
        assert!(ImageAsset::from_bytes(b"\x89PNG broken".to_vec()).is_err());
        let json = serde_json::to_value(&img).unwrap();
        let back: ImageAsset = serde_json::from_value(json).unwrap();
        assert_eq!(back, img);
        assert_eq!(
            decode_base64("data:image/png;base64,AAE=").unwrap(),
            vec![0, 1]
        );
    }

    /// A Radiance HDR file of `w` x `h` pixels from linear colours, flat or
    /// run-length encoded per channel.
    pub(crate) fn hdr_file(
        w: usize,
        h: usize,
        rle: bool,
        colour: impl Fn(usize, usize) -> [f32; 3],
    ) -> Vec<u8> {
        let mut out = format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {h} +X {w}\n").into_bytes();
        let rgbe = |c: [f32; 3]| -> [u8; 4] {
            let m = c[0].max(c[1]).max(c[2]);
            if m < 1e-32 {
                return [0; 4];
            }
            let e = m.log2().floor() as i32 + 1;
            let f = 256.0 / 2f32.powi(e);
            [
                (c[0] * f) as u8,
                (c[1] * f) as u8,
                (c[2] * f) as u8,
                (e + 128) as u8,
            ]
        };
        for y in 0..h {
            let row: Vec<[u8; 4]> = (0..w).map(|x| rgbe(colour(x, y))).collect();
            if rle {
                out.extend([2, 2, (w >> 8) as u8, w as u8]);
                for ch in 0..4 {
                    // One run of the first value, then the rest literally.
                    let first = row[0][ch];
                    let run = row.iter().take_while(|p| p[ch] == first).count().min(127);
                    if run > 1 {
                        out.extend([128 + run as u8, first]);
                    }
                    let start = if run > 1 { run } else { 0 };
                    for chunk in row[start..].chunks(128) {
                        out.push(chunk.len() as u8);
                        out.extend(chunk.iter().map(|p| p[ch]));
                    }
                }
            } else {
                out.extend(row.iter().flatten());
            }
        }
        out
    }

    #[test]
    fn reads_radiance_hdr_files() {
        let colour = |x: usize, y: usize| [x as f32 * 4.0 + 0.5, y as f32 * 0.25, 40.0];
        for rle in [false, true] {
            let file = hdr_file(12, 3, rle, colour);
            let img = ImageAsset::from_bytes(file.clone()).unwrap();
            assert_eq!(
                (img.mime.as_str(), img.width, img.height),
                (HDR_MIME, 12, 3)
            );
            img.validate().unwrap();
            let px = img.linear().unwrap();
            for y in 0..3 {
                for x in 0..12 {
                    let want = colour(x, y);
                    let got = px.rgb[y * 12 + x];
                    for k in 0..3 {
                        assert!(
                            (got[k] - want[k]).abs() <= want[0].max(want[1]).max(want[2]) / 100.0,
                            "{rle} {x},{y}: {got:?} vs {want:?}"
                        );
                    }
                }
            }
            // Clamped for textures and browsers.
            assert_eq!(img.decode().unwrap().rgb[0][2], 1.0);
            let (mime, png) = img.portable().unwrap();
            assert_eq!(mime, "image/png");
            assert_eq!(ImageAsset::from_bytes(png).unwrap().width, 12);
            // Cut short, it is an error rather than a panic.
            assert!(img.linear().is_ok());
            let short = ImageAsset {
                data: file[..file.len() - 9].to_vec().into(),
                ..img
            };
            assert!(short.linear().is_err());
        }
        assert!(ImageAsset::from_bytes(b"#?RADIANCE\n\n+Y 2 +X 2\n".to_vec()).is_err());
    }
}
