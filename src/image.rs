//! Image assets: PNG or JPEG files stored in the scene by name, so a scene
//! file, an undo step and a glTF round trip all carry them. Materials use
//! them as colour textures or normal maps (see `texture.rs`). The bytes are
//! kept as uploaded (and shared between undo snapshots); pixels are decoded
//! only where they are sampled.

use std::io::Cursor;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::engine::EngineError;

/// Largest accepted file, in bytes.
pub const MAX_IMAGE_BYTES: usize = 8 << 20;
/// Largest accepted width or height, in pixels.
pub const MAX_IMAGE_SIDE: u32 = 4096;
/// Most images one scene may hold.
pub const MAX_IMAGES: usize = 32;

fn err<T>(message: impl Into<String>) -> Result<T, EngineError> {
    Err(EngineError::new(message))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImageAsset {
    /// `image/png` or `image/jpeg`.
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
    /// Check a PNG or JPEG file and read its size (without decoding pixels).
    pub fn from_bytes(bytes: Vec<u8>) -> Result<ImageAsset, EngineError> {
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
            return err("images must be PNG or JPEG");
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

    /// Decode to sRGB pixels in 0..1.
    pub fn decode(&self) -> Result<Pixels, EngineError> {
        let bytes: &[u8] = &self.data;
        let fail = |e: String| EngineError::new(format!("cannot decode image: {e}"));
        let (width, height, rgb) = if self.mime == "image/png" {
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

/// Decoded sRGB pixels, row 0 at the top.
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
}
