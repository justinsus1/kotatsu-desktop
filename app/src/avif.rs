//! AVIF decoding (pure Rust via zenavif/rav1d-safe, no system libs) plus a
//! single `decode_image` entry point used everywhere instead of
//! `image::load_from_memory`.

use image::{DynamicImage, ImageBuffer};
use zenavif::{DecoderConfig, Unstoppable};
use zenpixels::{ChannelLayout, ChannelType};

/// ISO-BMFF `ftyp` box with an AVIF brand (`avif` still, `avis` sequence).
pub fn is_avif(data: &[u8]) -> bool {
    data.len() >= 12 && &data[4..8] == b"ftyp" && matches!(&data[8..12], b"avif" | b"avis")
}

/// Decode any supported page/cover image (jpeg, png, webp, gif, avif).
pub fn decode_image(data: &[u8]) -> Result<DynamicImage, String> {
    if is_avif(data) {
        return decode_avif(data);
    }
    image::load_from_memory(data).map_err(|e| e.to_string())
}

pub fn decode_avif(data: &[u8]) -> Result<DynamicImage, String> {
    // Image workers already run in parallel, so keep each decode single-threaded.
    let cfg = DecoderConfig::new().threads(1).prefer_8bit(true);
    let buf = zenavif::decode_with(data, &cfg, &Unstoppable).map_err(|e| format!("avif: {e}"))?;
    let (w, h) = (buf.width(), buf.height());
    let d = buf.descriptor();
    let layout = d.layout();
    let ct = d.channel_type();
    let raw = buf.copy_to_contiguous_bytes();
    let bad = || format!("avif: unexpected buffer size for {w}x{h} {layout:?} {ct:?}");
    match ct {
        ChannelType::U8 => Ok(match layout {
            ChannelLayout::Gray => DynamicImage::ImageLuma8(ImageBuffer::from_raw(w, h, raw).ok_or_else(bad)?),
            ChannelLayout::GrayAlpha => DynamicImage::ImageLumaA8(ImageBuffer::from_raw(w, h, raw).ok_or_else(bad)?),
            ChannelLayout::Rgb => DynamicImage::ImageRgb8(ImageBuffer::from_raw(w, h, raw).ok_or_else(bad)?),
            ChannelLayout::Rgba => DynamicImage::ImageRgba8(ImageBuffer::from_raw(w, h, raw).ok_or_else(bad)?),
            ChannelLayout::Bgra => {
                let mut raw = raw;
                raw.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
                DynamicImage::ImageRgba8(ImageBuffer::from_raw(w, h, raw).ok_or_else(bad)?)
            }
            other => return Err(format!("avif: unsupported layout {other:?}")),
        }),
        ChannelType::U16 => {
            let v: Vec<u16> = raw.chunks_exact(2).map(|b| u16::from_ne_bytes([b[0], b[1]])).collect();
            Ok(match layout {
                ChannelLayout::Gray => DynamicImage::ImageLuma16(ImageBuffer::from_raw(w, h, v).ok_or_else(bad)?),
                ChannelLayout::GrayAlpha => DynamicImage::ImageLumaA16(ImageBuffer::from_raw(w, h, v).ok_or_else(bad)?),
                ChannelLayout::Rgb => DynamicImage::ImageRgb16(ImageBuffer::from_raw(w, h, v).ok_or_else(bad)?),
                ChannelLayout::Rgba => DynamicImage::ImageRgba16(ImageBuffer::from_raw(w, h, v).ok_or_else(bad)?),
                other => return Err(format!("avif: unsupported layout {other:?}")),
            })
        }
        other => Err(format!("avif: unsupported channel type {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `KD_AVIF_SAMPLE=/path/to.avif cargo test avif -- --ignored`
    #[test]
    #[ignore]
    fn decodes_sample() {
        let p = std::env::var("KD_AVIF_SAMPLE").expect("KD_AVIF_SAMPLE");
        let data = std::fs::read(p).unwrap();
        assert!(is_avif(&data));
        let t = std::time::Instant::now();
        let img = decode_image(&data).unwrap();
        eprintln!("{}x{} {:?} in {:?}", img.width(), img.height(), img.color(), t.elapsed());
        assert!(img.width() > 0 && img.height() > 0);
        if let Ok(out) = std::env::var("KD_AVIF_OUT") {
            img.to_rgb8().save(out).unwrap();
        }
    }
}
