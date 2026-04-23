use crate::dtype::DType;
use crate::error::{PortalError, PortalResult};

/// Serialize state/action values with a timestamp against a dtype schema.
///
/// Wire format: `[u64 timestamp_us][field0 bytes][field1 bytes]...`, all
/// little-endian. Each field's byte width is the declared `DType`'s
/// `size_bytes()`. Caller must pass `values.len() == schema.len()`.
pub(crate) fn serialize_values(timestamp_us: u64, values: &[f64], schema: &[DType]) -> Vec<u8> {
    debug_assert_eq!(values.len(), schema.len());
    let payload_bytes: usize = schema.iter().map(|d| d.size_bytes()).sum();
    let mut buf = Vec::with_capacity(8 + payload_bytes);
    buf.extend_from_slice(&timestamp_us.to_le_bytes());
    for (v, dtype) in values.iter().zip(schema.iter()) {
        dtype.encode(*v, &mut buf);
    }
    buf
}

/// Deserialize bytes back to a timestamp and ordered values, widening each
/// field to `f64` per the dtype schema.
pub(crate) fn deserialize_values(data: &[u8], schema: &[DType]) -> PortalResult<(u64, Vec<f64>)> {
    let payload_bytes: usize = schema.iter().map(|d| d.size_bytes()).sum();
    let expected_len = 8 + payload_bytes;
    if data.len() != expected_len {
        return Err(PortalError::Deserialization(format!(
            "expected {} bytes, got {}",
            expected_len,
            data.len()
        )));
    }
    let timestamp_us = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let mut values = Vec::with_capacity(schema.len());
    let mut offset = 8;
    for dtype in schema.iter() {
        let width = dtype.size_bytes();
        let v = dtype.decode(&data[offset..offset + width])?;
        values.push(v);
        offset += width;
    }
    Ok((timestamp_us, values))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f64_roundtrip() {
        let ts = 1_713_300_000_000u64;
        let values = vec![1.0, 2.5, -3.14];
        let schema = vec![DType::F64, DType::F64, DType::F64];
        let bytes = serialize_values(ts, &values, &schema);
        let (ts2, values2) = deserialize_values(&bytes, &schema).unwrap();
        assert_eq!(ts, ts2);
        assert_eq!(values, values2);
    }

    #[test]
    fn mixed_dtype_roundtrip() {
        let ts = 42u64;
        let values = vec![1.5, 127.0, 1.0, 65535.0];
        let schema = vec![DType::F32, DType::I8, DType::Bool, DType::U16];
        let bytes = serialize_values(ts, &values, &schema);
        assert_eq!(bytes.len(), 8 + 4 + 1 + 1 + 2);
        let (ts2, values2) = deserialize_values(&bytes, &schema).unwrap();
        assert_eq!(ts, ts2);
        assert_eq!(values2, vec![1.5, 127.0, 1.0, 65535.0]);
    }

    #[test]
    fn empty_schema() {
        let ts = 42u64;
        let values: Vec<f64> = vec![];
        let schema: Vec<DType> = vec![];
        let bytes = serialize_values(ts, &values, &schema);
        assert_eq!(bytes.len(), 8);
        let (ts2, values2) = deserialize_values(&bytes, &schema).unwrap();
        assert_eq!(ts, ts2);
        assert!(values2.is_empty());
    }

    #[test]
    fn wrong_length_errors() {
        let bytes = vec![0u8; 10];
        let schema = vec![DType::F64];
        assert!(deserialize_values(&bytes, &schema).is_err());
    }

    #[test]
    fn int_values_saturate_on_send() {
        let schema = vec![DType::I8];
        let bytes = serialize_values(0, &[500.0], &schema);
        let (_, vals) = deserialize_values(&bytes, &schema).unwrap();
        assert_eq!(vals, vec![127.0]);
    }
}
