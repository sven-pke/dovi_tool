#![allow(dead_code)]

use std::io::{BufRead, ErrorKind, Read, Write};

use anyhow::{Result, bail};

use dolby_vision::av1::ITU_T35_DOVI_RPU_PAYLOAD_HEADER;
use dolby_vision::rpu::dovi_rpu::DoviRpu;

// ---------------------------------------------------------------------------
// OBU type constants (AV1 spec Table 5)
// ---------------------------------------------------------------------------
pub const OBU_SEQUENCE_HEADER: u8 = 1;
pub const OBU_TEMPORAL_DELIMITER: u8 = 2;
pub const OBU_FRAME_HEADER: u8 = 3;
pub const OBU_METADATA: u8 = 5;
pub const OBU_FRAME: u8 = 6;
pub const OBU_REDUNDANT_FRAME_HEADER: u8 = 7;

/// Metadata type for ITU-T T.35
pub const METADATA_TYPE_ITUT_T35: u64 = 4;

/// Dolby Vision T.35 country code (United States)
pub const DOVI_COUNTRY_CODE: u8 = 0xB5;

// ---------------------------------------------------------------------------
// Obu — a single parsed OBU with its complete raw bytes
// ---------------------------------------------------------------------------

/// A single parsed AV1 Open Bitstream Unit.
pub struct Obu {
    pub obu_type: u8,
    pub temporal_id: u8,
    pub spatial_id: u8,
    /// Decoded payload bytes (after header + LEB128 size).
    pub payload: Vec<u8>,
    /// Complete raw bytes of this OBU as it appeared on disk.
    /// Used for pass-through writing.
    pub raw_bytes: Vec<u8>,
}

impl Obu {
    /// Read one OBU from `reader`.  Returns `None` on clean EOF.
    ///
    /// Only supports the *Low Overhead Bitstream Format* where every OBU
    /// carries a size field (`obu_has_size_field == 1`).
    pub fn read_from<R: Read>(reader: &mut R) -> Result<Option<Self>> {
        // ---- header byte ----
        let mut header_byte = [0u8; 1];
        match reader.read_exact(&mut header_byte) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e.into()),
        }

        let byte = header_byte[0];
        if byte >> 7 != 0 {
            bail!("AV1 OBU forbidden bit is set (byte = 0x{byte:02X})");
        }

        let obu_type = (byte >> 3) & 0x0F;
        let has_extension = (byte >> 2) & 1 != 0;
        let has_size_field = (byte >> 1) & 1 != 0;

        let mut raw = vec![byte];
        let mut temporal_id = 0u8;
        let mut spatial_id = 0u8;

        // ---- optional extension header ----
        if has_extension {
            let mut ext = [0u8; 1];
            reader.read_exact(&mut ext)?;
            temporal_id = (ext[0] >> 5) & 0x07;
            spatial_id = (ext[0] >> 3) & 0x03;
            raw.push(ext[0]);
        }

        if !has_size_field {
            bail!(
                "OBU (type {obu_type}) has no size field; \
                 only Low Overhead Bitstream Format is supported"
            );
        }

        // ---- LEB128 payload size ----
        let payload_size = {
            let mut size: u64 = 0;
            let mut shift = 0u32;
            loop {
                let mut b = [0u8; 1];
                reader.read_exact(&mut b)?;
                raw.push(b[0]);
                size |= ((b[0] & 0x7F) as u64) << shift;
                shift += 7;
                if b[0] & 0x80 == 0 {
                    break;
                }
                if shift >= 56 {
                    bail!("LEB128 overflow while reading OBU size");
                }
            }
            size as usize
        };

        // ---- payload ----
        let payload_start = raw.len();
        raw.resize(payload_start + payload_size, 0);
        reader.read_exact(&mut raw[payload_start..])?;
        let payload = raw[payload_start..].to_vec();

        Ok(Some(Obu {
            obu_type,
            temporal_id,
            spatial_id,
            payload,
            raw_bytes: raw,
        }))
    }
}

// ---------------------------------------------------------------------------
// LEB128 encoding / decoding
// ---------------------------------------------------------------------------

/// Encode a `u64` value as LEB128 (unsigned).
pub fn encode_leb128(mut value: u64) -> Vec<u8> {
    let mut result = Vec::new();
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        result.push(byte);
        if value == 0 {
            break;
        }
    }
    result
}

/// Decode a LEB128-encoded value from `data`.
/// Returns `(value, bytes_consumed)`.
pub fn decode_leb128(data: &[u8]) -> (u64, usize) {
    let mut value = 0u64;
    let mut bytes_read = 0usize;
    for (i, &byte) in data.iter().enumerate() {
        if i >= 8 {
            break;
        }
        value |= ((byte & 0x7F) as u64) << (7 * i);
        bytes_read += 1;
        if byte & 0x80 == 0 {
            break;
        }
    }
    (value, bytes_read)
}

// ---------------------------------------------------------------------------
// Dolby Vision RPU detection
// ---------------------------------------------------------------------------

/// Returns the T.35 payload bytes (starting at `0xB5` country code) if this
/// `OBU_METADATA` payload contains a Dolby Vision RPU.
///
/// Layout after `metadata_type = 4` (LEB128):
/// ```text
/// country_code          (u8)      = 0xB5
/// terminal_provider_code (u16 BE) = 0x003B
/// terminal_provider_oriented_code (u32 BE) = 0x00000800
/// <EMDF container with RPU>
/// ```
pub fn extract_dovi_t35_payload(obu_payload: &[u8]) -> Option<&[u8]> {
    if obu_payload.is_empty() {
        return None;
    }

    // metadata_type (LEB128) must be 4
    let (mt, mt_len) = decode_leb128(obu_payload);
    if mt != METADATA_TYPE_ITUT_T35 {
        return None;
    }

    let t35 = &obu_payload[mt_len..];

    // Must start with Dolby Vision country code
    if t35.is_empty() || t35[0] != DOVI_COUNTRY_CODE {
        return None;
    }

    // After country code, the next bytes must match the Dolby Vision header
    let after_cc = &t35[1..];
    let hdr_len = ITU_T35_DOVI_RPU_PAYLOAD_HEADER.len();
    if after_cc.len() < hdr_len {
        return None;
    }

    if &after_cc[..hdr_len] == ITU_T35_DOVI_RPU_PAYLOAD_HEADER {
        Some(t35) // return slice starting at 0xB5
    } else {
        None
    }
}

/// Returns `true` if this OBU is an `OBU_METADATA` carrying a Dolby Vision RPU.
pub fn is_dovi_rpu_obu(obu: &Obu) -> bool {
    obu.obu_type == OBU_METADATA && extract_dovi_t35_payload(&obu.payload).is_some()
}

// ---------------------------------------------------------------------------
// Dolby Vision OBU encoding
// ---------------------------------------------------------------------------

/// Build a complete `OBU_METADATA` unit containing the Dolby Vision RPU.
///
/// Structure:
/// ```text
/// OBU header byte  = 0x2A  (type=5, has_size_field=1)
/// OBU size         (LEB128)
/// metadata_type    (LEB128) = 4
/// 0xB5             country_code
/// <EMDF-wrapped RPU payload>
/// ```
pub fn build_dovi_obu(rpu: &DoviRpu) -> Result<Vec<u8>> {
    // write_av1_rpu_metadata_obu_t35_complete returns: 0xB5 + EMDF payload
    let t35_complete = rpu.write_av1_rpu_metadata_obu_t35_complete()?;

    // OBU_METADATA payload: metadata_type(LEB128=4) + T.35 complete payload
    let mut obu_payload = encode_leb128(METADATA_TYPE_ITUT_T35);
    obu_payload.extend_from_slice(&t35_complete);

    // OBU header byte:
    //   bit 7:   forbidden = 0
    //   bits 6-3: obu_type = 5 (OBU_METADATA)
    //   bit 2:   obu_extension_flag = 0
    //   bit 1:   obu_has_size_field = 1
    //   bit 0:   reserved = 0
    // => (5 << 3) | 0x02 = 0x2A
    let header_byte = (OBU_METADATA << 3) | 0x02u8;
    let size_bytes = encode_leb128(obu_payload.len() as u64);

    let mut result = Vec::with_capacity(1 + size_bytes.len() + obu_payload.len());
    result.push(header_byte);
    result.extend_from_slice(&size_bytes);
    result.extend_from_slice(&obu_payload);

    Ok(result)
}

// ---------------------------------------------------------------------------
// IVF container support
// ---------------------------------------------------------------------------

/// IVF file signature ("DKIF").
pub const IVF_SIGNATURE: [u8; 4] = *b"DKIF";

/// Size of the IVF file header in bytes.
pub const IVF_FILE_HEADER_LEN: usize = 32;

/// Size of an IVF frame header in bytes.
pub const IVF_FRAME_HEADER_LEN: usize = 12;

/// Header of a single IVF frame.
pub struct IvfFrameHeader {
    /// Number of bytes in the frame data that follows.
    pub frame_size: u32,
    /// Presentation timestamp (in stream timebase).
    pub timestamp: u64,
}

/// Probe the first bytes of `reader` to decide whether the stream is an IVF
/// container. If the IVF signature is detected the 32-byte file header is
/// consumed from `reader` and returned; otherwise `None` is returned and
/// **no bytes are consumed**.
pub fn try_read_ivf_file_header<R: BufRead>(
    reader: &mut R,
) -> Result<Option<[u8; IVF_FILE_HEADER_LEN]>> {
    {
        let buf = reader.fill_buf()?;
        if buf.len() < 4 || buf[..4] != IVF_SIGNATURE {
            return Ok(None);
        }
    }
    let mut header = [0u8; IVF_FILE_HEADER_LEN];
    reader.read_exact(&mut header)?;
    Ok(Some(header))
}

/// Read one IVF frame header from `reader`. Returns `None` on clean EOF.
pub fn read_ivf_frame_header<R: Read>(reader: &mut R) -> Result<Option<IvfFrameHeader>> {
    let mut buf = [0u8; IVF_FRAME_HEADER_LEN];
    match reader.read_exact(&mut buf) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    Ok(Some(IvfFrameHeader {
        frame_size: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
        timestamp: u64::from_le_bytes(buf[4..12].try_into().unwrap()),
    }))
}

/// Write an IVF frame header (frame_size + timestamp) to `writer`.
pub fn write_ivf_frame_header<W: Write>(
    writer: &mut W,
    frame_size: u32,
    timestamp: u64,
) -> Result<()> {
    writer.write_all(&frame_size.to_le_bytes())?;
    writer.write_all(&timestamp.to_le_bytes())?;
    Ok(())
}

/// Read all OBUs from a single IVF frame's data bytes.
pub fn read_obus_from_ivf_frame(frame_data: Vec<u8>) -> Result<Vec<Obu>> {
    let mut cursor = std::io::Cursor::new(frame_data);
    let mut obus = Vec::new();
    while let Some(obu) = Obu::read_from(&mut cursor)? {
        obus.push(obu);
    }
    Ok(obus)
}
