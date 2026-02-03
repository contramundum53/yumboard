use bincode::{Decode, Encode};

use crate::Stroke;

pub const SESSION_FILE_MAGIC: [u8; 4] = *b"YBSS";
pub const SESSION_FILE_VERSION: u32 = 1;
const SESSION_HEADER_LEN: usize = SESSION_FILE_MAGIC.len() + std::mem::size_of::<u32>();

#[derive(Clone, Debug, Default, Encode, Decode, serde::Serialize, serde::Deserialize)]
pub struct SessionFileData {
    pub strokes: Vec<Stroke>,
}

#[derive(Debug)]
pub enum SessionFileDecodeError {
    UnsupportedVersion(u32),
    InvalidData,
}

pub fn encode_session_file(data: &SessionFileData) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&SESSION_FILE_MAGIC);
    payload.extend_from_slice(&SESSION_FILE_VERSION.to_le_bytes());
    let body = bincode::encode_to_vec(data, bincode::config::standard()).unwrap_or_default();
    payload.extend_from_slice(&body);
    payload
}

pub fn decode_session_file(payload: &[u8]) -> Result<SessionFileData, SessionFileDecodeError> {
    if !(payload.len() >= SESSION_HEADER_LEN && payload.starts_with(&SESSION_FILE_MAGIC)) {
        return Err(SessionFileDecodeError::InvalidData);
    }
    let version = u32::from_le_bytes(
        payload[SESSION_FILE_MAGIC.len()..SESSION_HEADER_LEN]
            .try_into()
            .map_err(|_| SessionFileDecodeError::InvalidData)?,
    );
    let body = &payload[SESSION_HEADER_LEN..];
    return match version {
        1 => bincode::decode_from_slice(body, bincode::config::standard())
            .map(|(data, _)| data)
            .map_err(|_| SessionFileDecodeError::InvalidData),
        _ => Err(SessionFileDecodeError::UnsupportedVersion(version)),
    };
}
