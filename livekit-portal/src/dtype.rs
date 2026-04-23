use crate::error::{PortalError, PortalResult};

/// Declared type for a single state or action field.
///
/// State and action schemas carry one `DType` per field. The declared type
/// drives wire-format width and value reconstruction. Values are carried as
/// `f64` through the Rust core and cast to the declared dtype only at the
/// serialization boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    F64,
    F32,
    I32,
    I16,
    I8,
    U32,
    U16,
    U8,
    Bool,
}

impl DType {
    /// Byte width on the wire.
    pub fn size_bytes(self) -> usize {
        match self {
            DType::F64 => 8,
            DType::F32 | DType::I32 | DType::U32 => 4,
            DType::I16 | DType::U16 => 2,
            DType::I8 | DType::U8 | DType::Bool => 1,
        }
    }

    /// Encode an `f64` into `out` at the declared width, saturating on
    /// overflow for integer dtypes. `Bool` treats any non-zero finite value
    /// as true.
    pub(crate) fn encode(self, v: f64, out: &mut Vec<u8>) {
        match self {
            DType::F64 => out.extend_from_slice(&v.to_le_bytes()),
            DType::F32 => out.extend_from_slice(&(v as f32).to_le_bytes()),
            DType::I32 => out.extend_from_slice(&saturate_i32(v).to_le_bytes()),
            DType::I16 => out.extend_from_slice(&saturate_i16(v).to_le_bytes()),
            DType::I8 => out.extend_from_slice(&saturate_i8(v).to_le_bytes()),
            DType::U32 => out.extend_from_slice(&saturate_u32(v).to_le_bytes()),
            DType::U16 => out.extend_from_slice(&saturate_u16(v).to_le_bytes()),
            DType::U8 => out.extend_from_slice(&saturate_u8(v).to_le_bytes()),
            DType::Bool => out.push(if v != 0.0 && !v.is_nan() { 1 } else { 0 }),
        }
    }

    /// Decode `buf[..self.size_bytes()]` into an `f64`.
    pub(crate) fn decode(self, buf: &[u8]) -> PortalResult<f64> {
        let need = self.size_bytes();
        if buf.len() < need {
            return Err(PortalError::Deserialization(format!(
                "dtype {self:?} needs {need} bytes, got {}",
                buf.len()
            )));
        }
        Ok(match self {
            DType::F64 => f64::from_le_bytes(buf[..8].try_into().unwrap()),
            DType::F32 => f32::from_le_bytes(buf[..4].try_into().unwrap()) as f64,
            DType::I32 => i32::from_le_bytes(buf[..4].try_into().unwrap()) as f64,
            DType::I16 => i16::from_le_bytes(buf[..2].try_into().unwrap()) as f64,
            DType::I8 => i8::from_le_bytes([buf[0]]) as f64,
            DType::U32 => u32::from_le_bytes(buf[..4].try_into().unwrap()) as f64,
            DType::U16 => u16::from_le_bytes(buf[..2].try_into().unwrap()) as f64,
            DType::U8 => buf[0] as f64,
            DType::Bool => {
                if buf[0] == 0 {
                    0.0
                } else {
                    1.0
                }
            }
        })
    }
}

fn saturate_i32(v: f64) -> i32 {
    if v.is_nan() {
        0
    } else if v >= i32::MAX as f64 {
        i32::MAX
    } else if v <= i32::MIN as f64 {
        i32::MIN
    } else {
        v as i32
    }
}

fn saturate_i16(v: f64) -> i16 {
    if v.is_nan() {
        0
    } else if v >= i16::MAX as f64 {
        i16::MAX
    } else if v <= i16::MIN as f64 {
        i16::MIN
    } else {
        v as i16
    }
}

fn saturate_i8(v: f64) -> i8 {
    if v.is_nan() {
        0
    } else if v >= i8::MAX as f64 {
        i8::MAX
    } else if v <= i8::MIN as f64 {
        i8::MIN
    } else {
        v as i8
    }
}

fn saturate_u32(v: f64) -> u32 {
    if v.is_nan() || v <= 0.0 {
        0
    } else if v >= u32::MAX as f64 {
        u32::MAX
    } else {
        v as u32
    }
}

fn saturate_u16(v: f64) -> u16 {
    if v.is_nan() || v <= 0.0 {
        0
    } else if v >= u16::MAX as f64 {
        u16::MAX
    } else {
        v as u16
    }
}

fn saturate_u8(v: f64) -> u8 {
    if v.is_nan() || v <= 0.0 {
        0
    } else if v >= u8::MAX as f64 {
        u8::MAX
    } else {
        v as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(dtype: DType, v: f64) -> f64 {
        let mut buf = Vec::new();
        dtype.encode(v, &mut buf);
        assert_eq!(buf.len(), dtype.size_bytes());
        dtype.decode(&buf).unwrap()
    }

    #[test]
    fn f64_roundtrip() {
        assert_eq!(roundtrip(DType::F64, 3.14159265358979), 3.14159265358979);
    }

    #[test]
    fn f32_lossy_roundtrip() {
        let r = roundtrip(DType::F32, 1.5);
        assert_eq!(r, 1.5);
    }

    #[test]
    fn int_saturates_on_overflow() {
        assert_eq!(roundtrip(DType::I8, 500.0), 127.0);
        assert_eq!(roundtrip(DType::I8, -500.0), -128.0);
        assert_eq!(roundtrip(DType::U8, -5.0), 0.0);
        assert_eq!(roundtrip(DType::U8, 1000.0), 255.0);
    }

    #[test]
    fn bool_maps_zero_and_nonzero() {
        assert_eq!(roundtrip(DType::Bool, 0.0), 0.0);
        assert_eq!(roundtrip(DType::Bool, 1.0), 1.0);
        assert_eq!(roundtrip(DType::Bool, -3.2), 1.0);
        assert_eq!(roundtrip(DType::Bool, f64::NAN), 0.0);
    }

    #[test]
    fn decode_buffer_too_short() {
        let buf = [0u8; 3];
        assert!(DType::F64.decode(&buf).is_err());
    }
}
