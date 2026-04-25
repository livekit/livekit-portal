//! Frame-video codecs: encode RGB24 to a payload byte string for byte-stream
//! transport, and decode a payload back to RGB24.
//!
//! The user-facing API takes and returns RGB regardless of codec or transport.
//! `Raw` is byte-for-byte RGB24, `Png` is RFC 2083, `Mjpeg` is one JPEG per
//! frame. PNG and JPEG carry their own dimensions so decode is self-describing;
//! `Raw` requires the caller to provide dimensions out-of-band.
//!
//! Quality is honored for `Mjpeg` (1..=100) and ignored for `Raw` and `Png`.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};

/// Codec used by a frame-video track.
///
/// Selected per-track at config time (see `PortalConfig::add_frame_video`).
/// Choice drives the wire size and CPU cost; the user-facing payload is RGB
/// in every case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    /// Uncompressed RGB24. Largest payload, zero encode cost. Use when CPU is
    /// scarce or you want bit-exact frames with no codec dependency.
    Raw,
    /// PNG, lossless. ~2-3x compression on natural images. ~10-30 ms encode at
    /// 480p.
    Png,
    /// Motion JPEG, lossy. ~10-20x compression at quality 90. Sub-millisecond
    /// decode. Each frame is an independent JPEG (no temporal coding), so
    /// frame loss is contained.
    Mjpeg,
}

impl Codec {
    /// Whether `quality` is meaningful for this codec. `false` means the
    /// caller's value is ignored on encode.
    pub fn uses_quality(self) -> bool {
        matches!(self, Codec::Mjpeg)
    }
}

/// Decoded frame: RGB24 bytes plus the dimensions parsed from the payload (or
/// echoed back, in `Raw`'s case).
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("invalid frame dimensions: {width}x{height}")]
    InvalidDimensions { width: u32, height: u32 },
    #[error(
        "wrong RGB buffer size for {width}x{height}: expected {expected} bytes, got {got}"
    )]
    WrongRgbSize { width: u32, height: u32, expected: usize, got: usize },
    #[error("invalid quality {0}: must be in 1..=100")]
    InvalidQuality(u8),
    #[error("encode failed: {0}")]
    EncodeFailed(String),
    #[error("decode failed: {0}")]
    DecodeFailed(String),
    #[error(
        "decoded dimensions {decoded_width}x{decoded_height} disagree with declared {declared_width}x{declared_height}"
    )]
    DimensionMismatch {
        decoded_width: u32,
        decoded_height: u32,
        declared_width: u32,
        declared_height: u32,
    },
}

pub type CodecResult<T> = Result<T, CodecError>;

fn check_rgb(rgb: &[u8], width: u32, height: u32) -> CodecResult<usize> {
    if width == 0 || height == 0 {
        return Err(CodecError::InvalidDimensions { width, height });
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(3))
        .ok_or(CodecError::InvalidDimensions { width, height })?;
    if rgb.len() != expected {
        return Err(CodecError::WrongRgbSize {
            width,
            height,
            expected,
            got: rgb.len(),
        });
    }
    Ok(expected)
}

/// Encode an RGB24 frame to the wire payload for `codec`. `quality` is in
/// `1..=100` for `Mjpeg` and is ignored for `Raw` and `Png`.
pub fn encode_frame(
    rgb: &[u8],
    width: u32,
    height: u32,
    codec: Codec,
    quality: u8,
) -> CodecResult<Vec<u8>> {
    check_rgb(rgb, width, height)?;
    match codec {
        Codec::Raw => Ok(rgb.to_vec()),
        Codec::Png => {
            // Default filter + fast compression. PNG's strength is "good
            // enough lossless"; cranking compression past Default doesn't
            // change the bitstream-level losslessness, only the encode cost.
            let mut out = Vec::new();
            let encoder = PngEncoder::new_with_quality(
                &mut out,
                CompressionType::Default,
                PngFilterType::Adaptive,
            );
            encoder
                .write_image(rgb, width, height, ExtendedColorType::Rgb8)
                .map_err(|e| CodecError::EncodeFailed(e.to_string()))?;
            Ok(out)
        }
        Codec::Mjpeg => {
            if !(1..=100).contains(&quality) {
                return Err(CodecError::InvalidQuality(quality));
            }
            let mut out = Vec::new();
            let mut encoder = JpegEncoder::new_with_quality(&mut out, quality);
            encoder
                .encode(rgb, width, height, ExtendedColorType::Rgb8)
                .map_err(|e| CodecError::EncodeFailed(e.to_string()))?;
            Ok(out)
        }
    }
}

/// Decode a wire payload to RGB24 plus its dimensions. For `Raw`, dimensions
/// must be supplied by the caller via `declared_width` / `declared_height`
/// (the byte stream carries them in the framing header). For `Png` /
/// `Mjpeg`, the encoded bitstream carries its own dimensions; the declared
/// values, when non-zero, are checked against the decoded values and a
/// mismatch returns `DimensionMismatch`.
pub fn decode_frame(
    bytes: &[u8],
    codec: Codec,
    declared_width: u32,
    declared_height: u32,
) -> CodecResult<DecodedFrame> {
    match codec {
        Codec::Raw => {
            check_rgb(bytes, declared_width, declared_height)?;
            Ok(DecodedFrame {
                rgb: bytes.to_vec(),
                width: declared_width,
                height: declared_height,
            })
        }
        Codec::Png => decode_with_image_crate(
            bytes,
            image::ImageFormat::Png,
            declared_width,
            declared_height,
        ),
        Codec::Mjpeg => decode_with_image_crate(
            bytes,
            image::ImageFormat::Jpeg,
            declared_width,
            declared_height,
        ),
    }
}

fn decode_with_image_crate(
    bytes: &[u8],
    format: image::ImageFormat,
    declared_width: u32,
    declared_height: u32,
) -> CodecResult<DecodedFrame> {
    let cursor = Cursor::new(bytes);
    let reader = image::ImageReader::with_format(cursor, format);
    let decoded = reader
        .decode()
        .map_err(|e| CodecError::DecodeFailed(e.to_string()))?
        .into_rgb8();
    let (w, h) = decoded.dimensions();
    if (declared_width != 0 || declared_height != 0)
        && (w != declared_width || h != declared_height)
    {
        return Err(CodecError::DimensionMismatch {
            decoded_width: w,
            decoded_height: h,
            declared_width,
            declared_height,
        });
    }
    Ok(DecodedFrame { rgb: decoded.into_raw(), width: w, height: h })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                out.push((x % 256) as u8);
                out.push((y % 256) as u8);
                out.push(((x + y) % 256) as u8);
            }
        }
        out
    }

    #[test]
    fn raw_roundtrip_is_byte_exact() {
        let rgb = gradient(64, 48);
        let bytes = encode_frame(&rgb, 64, 48, Codec::Raw, 0).unwrap();
        assert_eq!(bytes, rgb, "raw encode is identity");
        let decoded = decode_frame(&bytes, Codec::Raw, 64, 48).unwrap();
        assert_eq!(decoded.rgb, rgb);
        assert_eq!((decoded.width, decoded.height), (64, 48));
    }

    #[test]
    fn png_roundtrip_is_byte_exact() {
        let rgb = gradient(64, 48);
        let bytes = encode_frame(&rgb, 64, 48, Codec::Png, 0).unwrap();
        assert!(bytes.len() < rgb.len() * 2, "png shouldn't grow much over raw");
        let decoded = decode_frame(&bytes, Codec::Png, 64, 48).unwrap();
        assert_eq!(decoded.rgb, rgb, "PNG is lossless");
        assert_eq!((decoded.width, decoded.height), (64, 48));
    }

    #[test]
    fn mjpeg_roundtrip_is_close() {
        let rgb = gradient(64, 48);
        let bytes = encode_frame(&rgb, 64, 48, Codec::Mjpeg, 95).unwrap();
        assert!(bytes.len() < rgb.len(), "jpeg should shrink the payload");
        let decoded = decode_frame(&bytes, Codec::Mjpeg, 64, 48).unwrap();
        assert_eq!((decoded.width, decoded.height), (64, 48));
        // JPEG is lossy; check the average per-pixel error is small.
        let total: u64 =
            rgb.iter().zip(decoded.rgb.iter()).map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs() as u64).sum();
        let avg = total as f64 / rgb.len() as f64;
        assert!(avg < 5.0, "avg pixel error {avg} should be small at q=95");
    }

    #[test]
    fn declared_dims_zero_skips_check() {
        // PNG / MJPEG payloads carry their own dimensions; passing 0/0 means
        // "trust the bitstream" which is what receivers do when they have no
        // out-of-band hint.
        let rgb = gradient(32, 32);
        let bytes = encode_frame(&rgb, 32, 32, Codec::Png, 0).unwrap();
        let decoded = decode_frame(&bytes, Codec::Png, 0, 0).unwrap();
        assert_eq!((decoded.width, decoded.height), (32, 32));
    }

    #[test]
    fn dimension_mismatch_rejected() {
        let rgb = gradient(32, 32);
        let bytes = encode_frame(&rgb, 32, 32, Codec::Png, 0).unwrap();
        let err = decode_frame(&bytes, Codec::Png, 64, 64).unwrap_err();
        assert!(matches!(err, CodecError::DimensionMismatch { .. }));
    }

    #[test]
    fn invalid_jpeg_quality_rejected() {
        let rgb = gradient(8, 8);
        assert!(matches!(
            encode_frame(&rgb, 8, 8, Codec::Mjpeg, 0),
            Err(CodecError::InvalidQuality(0))
        ));
        assert!(matches!(
            encode_frame(&rgb, 8, 8, Codec::Mjpeg, 101),
            Err(CodecError::InvalidQuality(101))
        ));
    }

    #[test]
    fn wrong_rgb_size_rejected() {
        let rgb = vec![0u8; 100]; // not 32*32*3
        let err = encode_frame(&rgb, 32, 32, Codec::Raw, 0).unwrap_err();
        assert!(matches!(err, CodecError::WrongRgbSize { .. }));
    }

    #[test]
    fn quality_only_meaningful_for_mjpeg() {
        assert!(!Codec::Raw.uses_quality());
        assert!(!Codec::Png.uses_quality());
        assert!(Codec::Mjpeg.uses_quality());
    }
}
