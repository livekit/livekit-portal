//! `ActionChunk`: a typed, policy-emitted block of future actions delivered
//! over a reserved byte-stream topic. Portal carries the bytes; users own
//! the RTC state machine (request flow, splicing, prev-chunk caching).
//!
//! On-wire layout (little-endian):
//!
//! ```text
//! 0..4   magic   "PCHK"
//! 4      version 0x01
//! 5      dtype   0=F32, 1=F16
//! 6..8   horizon            u16
//! 8..10  action_dim         u16
//! 10..18 captured_at_us     u64
//! 18..   payload            H * K * sizeof(dtype) bytes
//! ```
//!
//! Structural validation (payload length matches `H * K * sizeof(dtype)`)
//! runs at deserialize time. Semantic agreement (joint names, dtype choice,
//! horizon bounds) is the user's responsibility — Portal does not negotiate
//! a chunk schema.

use crate::error::{PortalError, PortalResult};

pub(crate) const CHUNK_TOPIC: &str = "portal_action_chunk";
const MAGIC: &[u8; 4] = b"PCHK";
const VERSION: u8 = 0x01;
const HEADER_LEN: usize = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkDtype {
    F32,
    F16,
}

impl ChunkDtype {
    pub fn size(self) -> usize {
        match self {
            ChunkDtype::F32 => 4,
            ChunkDtype::F16 => 2,
        }
    }

    fn code(self) -> u8 {
        match self {
            ChunkDtype::F32 => 0,
            ChunkDtype::F16 => 1,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(ChunkDtype::F32),
            1 => Some(ChunkDtype::F16),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActionChunk {
    pub horizon: u16,
    pub action_dim: u16,
    pub dtype: ChunkDtype,
    pub captured_at_us: u64,
    /// Packed little-endian bytes, length `horizon * action_dim * dtype.size()`.
    pub payload: Vec<u8>,
}

impl ActionChunk {
    pub fn expected_payload_len(&self) -> usize {
        self.horizon as usize * self.action_dim as usize * self.dtype.size()
    }
}

pub(crate) fn serialize_chunk(chunk: &ActionChunk) -> PortalResult<Vec<u8>> {
    let expected = chunk.expected_payload_len();
    if chunk.payload.len() != expected {
        return Err(PortalError::Deserialization(format!(
            "chunk payload length {} does not match H*K*dtype ({} * {} * {} = {})",
            chunk.payload.len(),
            chunk.horizon,
            chunk.action_dim,
            chunk.dtype.size(),
            expected,
        )));
    }

    let mut out = Vec::with_capacity(HEADER_LEN + expected);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(chunk.dtype.code());
    out.extend_from_slice(&chunk.horizon.to_le_bytes());
    out.extend_from_slice(&chunk.action_dim.to_le_bytes());
    out.extend_from_slice(&chunk.captured_at_us.to_le_bytes());
    out.extend_from_slice(&chunk.payload);
    Ok(out)
}

pub(crate) fn deserialize_chunk(bytes: &[u8]) -> PortalResult<ActionChunk> {
    if bytes.len() < HEADER_LEN {
        return Err(PortalError::Deserialization(format!(
            "chunk frame too short: {} < {HEADER_LEN}",
            bytes.len()
        )));
    }
    if &bytes[0..4] != MAGIC {
        return Err(PortalError::Deserialization(
            "chunk frame magic mismatch".to_string(),
        ));
    }
    if bytes[4] != VERSION {
        return Err(PortalError::Deserialization(format!(
            "chunk frame version {} unsupported (want {VERSION})",
            bytes[4]
        )));
    }
    let dtype = ChunkDtype::from_code(bytes[5]).ok_or_else(|| {
        PortalError::Deserialization(format!("unknown chunk dtype code {}", bytes[5]))
    })?;
    let horizon = u16::from_le_bytes([bytes[6], bytes[7]]);
    let action_dim = u16::from_le_bytes([bytes[8], bytes[9]]);
    let captured_at_us = u64::from_le_bytes([
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17],
    ]);
    let expected = horizon as usize * action_dim as usize * dtype.size();
    let payload = &bytes[HEADER_LEN..];
    if payload.len() != expected {
        return Err(PortalError::Deserialization(format!(
            "chunk payload length {} does not match header ({} * {} * {} = {})",
            payload.len(),
            horizon,
            action_dim,
            dtype.size(),
            expected,
        )));
    }
    Ok(ActionChunk {
        horizon,
        action_dim,
        dtype,
        captured_at_us,
        payload: payload.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_f32_chunk() {
        let payload: Vec<u8> = (0..50 * 7 * 4).map(|i| (i % 251) as u8).collect();
        let chunk = ActionChunk {
            horizon: 50,
            action_dim: 7,
            dtype: ChunkDtype::F32,
            captured_at_us: 1_234_567_890,
            payload: payload.clone(),
        };
        let frame = serialize_chunk(&chunk).unwrap();
        let back = deserialize_chunk(&frame).unwrap();
        assert_eq!(back.horizon, 50);
        assert_eq!(back.action_dim, 7);
        assert_eq!(back.dtype, ChunkDtype::F32);
        assert_eq!(back.captured_at_us, 1_234_567_890);
        assert_eq!(back.payload, payload);
    }

    #[test]
    fn round_trips_f16_chunk() {
        let payload: Vec<u8> = (0..16 * 14 * 2).map(|i| (i % 251) as u8).collect();
        let chunk = ActionChunk {
            horizon: 16,
            action_dim: 14,
            dtype: ChunkDtype::F16,
            captured_at_us: 42,
            payload: payload.clone(),
        };
        let frame = serialize_chunk(&chunk).unwrap();
        let back = deserialize_chunk(&frame).unwrap();
        assert_eq!(back.dtype, ChunkDtype::F16);
        assert_eq!(back.payload, payload);
    }

    #[test]
    fn rejects_payload_length_mismatch_on_serialize() {
        let chunk = ActionChunk {
            horizon: 10,
            action_dim: 7,
            dtype: ChunkDtype::F32,
            captured_at_us: 0,
            payload: vec![0u8; 10], // too short
        };
        assert!(serialize_chunk(&chunk).is_err());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut frame = serialize_chunk(&ActionChunk {
            horizon: 2,
            action_dim: 2,
            dtype: ChunkDtype::F32,
            captured_at_us: 0,
            payload: vec![0u8; 16],
        })
        .unwrap();
        frame[0] = b'X';
        assert!(deserialize_chunk(&frame).is_err());
    }

    #[test]
    fn rejects_payload_length_mismatch_on_deserialize() {
        let mut frame = serialize_chunk(&ActionChunk {
            horizon: 2,
            action_dim: 2,
            dtype: ChunkDtype::F32,
            captured_at_us: 0,
            payload: vec![0u8; 16],
        })
        .unwrap();
        frame.push(0x00); // trailing byte breaks expected length
        assert!(deserialize_chunk(&frame).is_err());
    }
}
