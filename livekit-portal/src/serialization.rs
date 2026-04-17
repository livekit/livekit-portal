use crate::error::{PortalError, PortalResult};

/// Serialize state/action values with a timestamp.
///
/// Wire format: `[u64 timestamp_us][f64 val0][f64 val1]...[f64 valN]`, all little-endian.
pub(crate) fn serialize_values(timestamp_us: u64, values: &[f64]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 + values.len() * 8);
    buf.extend_from_slice(&timestamp_us.to_le_bytes());
    for v in values {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf
}

/// Deserialize bytes back to a timestamp and ordered values.
pub(crate) fn deserialize_values(
    data: &[u8],
    expected_fields: usize,
) -> PortalResult<(u64, Vec<f64>)> {
    let expected_len = 8 + expected_fields * 8;
    if data.len() != expected_len {
        return Err(PortalError::Deserialization(format!(
            "expected {} bytes, got {}",
            expected_len,
            data.len()
        )));
    }
    let timestamp_us = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let mut values = Vec::with_capacity(expected_fields);
    for i in 0..expected_fields {
        let offset = 8 + i * 8;
        let v = f64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
        values.push(v);
    }
    Ok((timestamp_us, values))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let ts = 1_713_300_000_000u64;
        let values = vec![1.0, 2.5, -3.14];
        let bytes = serialize_values(ts, &values);
        let (ts2, values2) = deserialize_values(&bytes, 3).unwrap();
        assert_eq!(ts, ts2);
        assert_eq!(values, values2);
    }

    #[test]
    fn empty_values() {
        let ts = 42u64;
        let values: Vec<f64> = vec![];
        let bytes = serialize_values(ts, &values);
        assert_eq!(bytes.len(), 8);
        let (ts2, values2) = deserialize_values(&bytes, 0).unwrap();
        assert_eq!(ts, ts2);
        assert!(values2.is_empty());
    }

    #[test]
    fn wrong_length() {
        let bytes = vec![0u8; 10];
        assert!(deserialize_values(&bytes, 1).is_err());
    }
}
