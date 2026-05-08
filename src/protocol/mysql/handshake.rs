#![allow(unused)]

use bytes::{BufMut, BytesMut};

use crate::protocol::error::Result;
use crate::protocol::error::ProtocolError;

pub mod capability {
    pub const LONG_PASSWORD: u32 = 1 << 0;
    pub const FOUND_ROWS: u32 = 1 << 1;
    pub const LONG_FLAG: u32 = 1 << 2;
    pub const CONNECT_WITH_DB: u32 = 1 << 3;
    pub const NO_SCHEMA: u32 = 1 << 4;
    pub const COMPRESS: u32 = 1 << 5;
    pub const ODBC: u32 = 1 << 6;
    pub const LOCAL_FILES: u32 = 1 << 7;
    pub const IGNORE_SPACE: u32 = 1 << 8;
    pub const PROTOCOL_41: u32 = 1 << 9;
    pub const INTERACTIVE: u32 = 1 << 10;
    pub const SSL: u32 = 1 << 11;
    pub const IGNORE_SIGPIPE: u32 = 1 << 12;
    pub const TRANSACTIONS: u32 = 1 << 13;
    pub const SECURE_CONNECTION: u32 = 1 << 15;
    pub const MULTI_STATEMENTS: u32 = 1 << 16;
    pub const MULTI_RESULTS: u32 = 1 << 17;
    pub const PS_MULTI_RESULTS: u32 = 1 << 18;
    pub const PLUGIN_AUTH: u32 = 1 << 19;
    pub const CONNECT_ATTRS: u32 = 1 << 20;
    pub const PLUGIN_AUTH_LENENC_CLIENT_DATA: u32 = 1 << 21;
    pub const CLIENT_SESSION_TRACK: u32 = 1 << 23;
    pub const DEPRECATE_EOF: u32 = 1 << 24;
}

pub struct HandshakeV10 {
    pub protocol_version: u8,
    pub server_version: String,
    pub connection_id: u32,
    pub auth_plugin_data_1: [u8; 8],
    pub auth_plugin_data_2: [u8; 12],
    pub capability_flags: u32,
    pub character_set: u8,
    pub status_flags: u16,
    pub auth_plugin_name: String,
}

impl HandshakeV10 {
    pub fn encode(&self) -> BytesMut {
        let mut buf = BytesMut::new();

        buf.put_u8(self.protocol_version);
        buf.put_slice(self.server_version.as_bytes());
        buf.put_u8(0);
        buf.put_u32_le(self.connection_id);
        buf.put_slice(&self.auth_plugin_data_1);
        buf.put_u8(0);
        buf.put_u16_le((self.capability_flags & 0xFFFF) as u16);
        buf.put_u8(self.character_set);
        buf.put_u16_le(self.status_flags);
        buf.put_u16_le(((self.capability_flags >> 16) & 0xFFFF) as u16);
        buf.put_u8(21); // auth_plugin_data_len: 8 + 12 + 1
        buf.put_slice(&[0u8; 10]);
        buf.put_slice(&self.auth_plugin_data_2);
        buf.put_u8(0);
        buf.put_slice(self.auth_plugin_name.as_bytes());
        buf.put_u8(0);

        buf
    }
}

pub struct HandshakeResponse41 {
    pub capability_flags: u32,
    pub max_packet_size: u32,
    pub character_set: u8,
    pub username: String,
    pub auth_response: Vec<u8>,
    pub database: Option<String>,
    pub auth_plugin_name: Option<String>,
    /// `_program_name` from the MySQL connection attributes block, if sent.
    pub app_name: Option<String>,
}

impl HandshakeResponse41 {
    pub fn decode(mut buf: &[u8]) -> Result<Self> {
        use bytes::Buf;

        if buf.remaining() < 32 {
            return Err(ProtocolError::InvalidFormat(
                "HandshakeResponse41 too short".into(),
            ));
        }

        let capability_flags = buf.get_u32_le();
        let max_packet_size = buf.get_u32_le();
        let character_set = buf.get_u8();

        if buf.remaining() < 23 {
            return Err(ProtocolError::InvalidFormat(
                "HandshakeResponse41 missing reserved bytes".into(),
            ));
        }
        buf.advance(23);

        let username = read_null_terminated_string(&mut buf)?;

        let auth_response_len = buf.get_u8() as usize;
        let mut auth_response = vec![0u8; auth_response_len];
        if buf.remaining() < auth_response_len {
            return Err(ProtocolError::InvalidFormat(
                "HandshakeResponse41 auth response truncated".into(),
            ));
        }
        buf.copy_to_slice(&mut auth_response);

        let mut database = None;
        if (capability_flags & capability::CONNECT_WITH_DB) != 0 {
            database = Some(read_null_terminated_string(&mut buf)?);
        }

        let mut auth_plugin_name = None;
        if (capability_flags & capability::PLUGIN_AUTH) != 0 {
            auth_plugin_name = Some(read_null_terminated_string(&mut buf)?);
        }

        // Parse connection attributes to extract _program_name.
        let mut app_name = None;
        if (capability_flags & capability::CONNECT_ATTRS) != 0 && !buf.is_empty() {
            app_name = parse_app_name_from_attrs(buf);
        }

        Ok(Self {
            capability_flags,
            max_packet_size,
            character_set,
            username,
            auth_response,
            database,
            auth_plugin_name,
            app_name,
        })
    }

    /// Re-encode a HandshakeResponse41 for forwarding to the backend.
    pub fn encode(&self) -> BytesMut {
        let mut buf = BytesMut::new();

        buf.put_u32_le(self.capability_flags);
        buf.put_u32_le(self.max_packet_size);
        buf.put_u8(self.character_set);
        buf.put_slice(&[0u8; 23]);

        buf.put_slice(self.username.as_bytes());
        buf.put_u8(0);

        buf.put_u8(self.auth_response.len() as u8);
        buf.put_slice(&self.auth_response);

        if let Some(ref db) = self.database {
            buf.put_slice(db.as_bytes());
            buf.put_u8(0);
        }

        if let Some(ref plugin) = self.auth_plugin_name {
            buf.put_slice(plugin.as_bytes());
            buf.put_u8(0);
        }

        buf
    }
}

fn read_null_terminated_string(buf: &mut &[u8]) -> Result<String> {
    let null_pos = buf
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| {
            ProtocolError::InvalidFormat(
                "Missing null terminator in string".into(),
            )
        })?;

    let s = String::from_utf8_lossy(&buf[..null_pos]).into_owned();
    *buf = &buf[null_pos + 1..];
    Ok(s)
}

/// Decode a length-encoded integer from `buf`, advancing past it.
/// Returns `None` if the buffer is empty or malformed.
fn read_lenenc_int(buf: &mut &[u8]) -> Option<usize> {
    use bytes::Buf;
    if buf.is_empty() { return None; }
    let first = buf[0];
    *buf = &buf[1..];
    match first {
        0xfb => None, // NULL
        0xfc => {
            if buf.len() < 2 { return None; }
            let v = u16::from_le_bytes([buf[0], buf[1]]) as usize;
            *buf = &buf[2..];
            Some(v)
        }
        0xfd => {
            if buf.len() < 3 { return None; }
            let v = u32::from_le_bytes([buf[0], buf[1], buf[2], 0]) as usize;
            *buf = &buf[3..];
            Some(v)
        }
        0xfe => {
            if buf.len() < 8 { return None; }
            let v = u64::from_le_bytes([
                buf[0], buf[1], buf[2], buf[3],
                buf[4], buf[5], buf[6], buf[7],
            ]) as usize;
            *buf = &buf[8..];
            Some(v)
        }
        n => Some(n as usize),
    }
}

/// Parse the MySQL connection_attributes block and return the value of
/// `_program_name` if present. Never fails — returns `None` on any parse error.
fn parse_app_name_from_attrs(buf: &[u8]) -> Option<String> {
    let mut remaining = buf;
    // First byte(s): total byte length of the attributes block (lenenc int).
    let total_len = read_lenenc_int(&mut remaining)?;
    if remaining.len() < total_len { return None; }
    let mut attrs = &remaining[..total_len];

    while !attrs.is_empty() {
        let key_len = read_lenenc_int(&mut attrs)?;
        if attrs.len() < key_len { break; }
        let key = std::str::from_utf8(&attrs[..key_len]).unwrap_or("");
        attrs = &attrs[key_len..];

        let val_len = read_lenenc_int(&mut attrs)?;
        if attrs.len() < val_len { break; }
        let val = std::str::from_utf8(&attrs[..val_len]).unwrap_or("");
        attrs = &attrs[val_len..];

        if key == "_program_name" && !val.is_empty() {
            return Some(val.to_string());
        }
    }
    None
}
