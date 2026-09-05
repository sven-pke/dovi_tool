// Re-export everything the rest of the codebase uses from the av1_parser crate
#[allow(unused_imports)]
pub use av1_parser::{
    IVF_SIGNATURE, IvfFrameHeader, IvfWriter, OBU_FRAME, OBU_FRAME_HEADER, OBU_METADATA,
    OBU_REDUNDANT_FRAME_HEADER, OBU_SEQUENCE_HEADER, OBU_TEMPORAL_DELIMITER, Obu, ObuReader,
    ObuWriter, decode_leb128, encode_leb128, read_ivf_frame_header, read_obus_from_ivf_frame,
    try_read_ivf_file_header, write_ivf_frame_header,
};

use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::Path;

use anyhow::Result;
use dolby_vision::rpu::dovi_rpu::DoviRpu;

use dolby_vision::av1::ITU_T35_DOVI_RPU_PAYLOAD_HEADER;

/// Metadata type for ITU-T T.35
const METADATA_TYPE_ITUT_T35: u64 = 4;

/// Dolby Vision T.35 country code (United States)
const DOVI_COUNTRY_CODE: u8 = 0xB5;

/// `trailing_bits()` for an OBU that ends on a byte boundary: a single
/// `trailing_one_bit` followed by seven `trailing_zero_bit`s (AV1 spec 5.3.4).
///
/// Every OBU other than `OBU_TILE_GROUP`, `OBU_TILE_LIST` and `OBU_FRAME` must
/// end with these, and the T.35 payload of a Dolby Vision RPU is byte aligned,
/// so the trailing bits always occupy one extra byte. Without it strict parsers
/// such as FFmpeg's `cbs_av1` reject the unit with
/// "trailing_one_bit out of range: 0".
const OBU_TRAILING_BITS_BYTE: u8 = 0x80;

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

/// Build a complete `OBU_METADATA` unit containing the Dolby Vision RPU.
///
/// Structure:
/// ```text
/// OBU header byte  = 0x2A  (type=5, has_size_field=1)
/// OBU size         (LEB128)
/// metadata_type    (LEB128) = 4
/// 0xB5             country_code
/// <EMDF-wrapped RPU payload>
/// 0x80             trailing_bits()
/// ```
pub fn build_dovi_obu(rpu: &DoviRpu) -> Result<Vec<u8>> {
    // write_av1_rpu_metadata_obu_t35_complete returns: 0xB5 + EMDF payload
    let t35_complete = rpu.write_av1_rpu_metadata_obu_t35_complete()?;

    // OBU_METADATA payload: metadata_type(LEB128=4) + T.35 complete payload
    let mut obu_payload = encode_leb128(METADATA_TYPE_ITUT_T35);
    obu_payload.extend_from_slice(&t35_complete);

    // trailing_bits() — required for every OBU that is not a tile group/list or frame
    obu_payload.push(OBU_TRAILING_BITS_BYTE);

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

/// Position within a temporal unit at which per-frame metadata OBUs belong:
/// immediately before the first frame or frame header OBU, therefore *after*
/// the temporal delimiter, the sequence header and any static metadata.
///
/// Inserting earlier (right behind the temporal delimiter) makes muxers treat
/// the RPU as part of the AV1 codec configuration record — mkvmerge copies it
/// into `CodecPrivate`, where per-frame metadata has no business being.
pub fn metadata_insert_index(obus: &[Obu]) -> usize {
    obus.iter()
        .position(|o| {
            matches!(
                o.obu_type,
                OBU_FRAME | OBU_FRAME_HEADER | OBU_REDUNDANT_FRAME_HEADER
            )
        })
        .unwrap_or(obus.len())
}

// ---------------------------------------------------------------------------
// Codec detection
// ---------------------------------------------------------------------------

/// Which elementary stream syntax an input uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitstreamCodec {
    Hevc,
    Av1,
}

/// Number of leading bytes examined when guessing the codec.
const SNIFF_LEN: usize = 16;

/// Guess the codec from the first bytes of an elementary stream.
///
/// Recognises the IVF signature and a plausible leading AV1 OBU (temporal
/// delimiter or sequence header, low overhead format). Everything else —
/// including HEVC Annex B start codes — is reported as HEVC, which keeps the
/// previous behaviour for anything unexpected.
pub fn sniff_codec(buf: &[u8]) -> BitstreamCodec {
    if buf.len() >= 4 && buf[..4] == IVF_SIGNATURE {
        return BitstreamCodec::Av1;
    }

    // HEVC Annex B start code
    if buf.starts_with(&[0x00, 0x00, 0x01]) || buf.starts_with(&[0x00, 0x00, 0x00, 0x01]) {
        return BitstreamCodec::Hevc;
    }

    if let Some(&first) = buf.first() {
        let forbidden = first >> 7;
        let obu_type = (first >> 3) & 0x0F;
        let has_size_field = (first >> 1) & 1;

        if forbidden == 0
            && has_size_field == 1
            && matches!(obu_type, OBU_TEMPORAL_DELIMITER | OBU_SEQUENCE_HEADER)
        {
            return BitstreamCodec::Av1;
        }
    }

    BitstreamCodec::Hevc
}

/// Codec implied by a file extension, if it is unambiguous.
pub fn codec_from_extension(path: &Path) -> Option<BitstreamCodec> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("av1") | Some("ivf") | Some("obu") => Some(BitstreamCodec::Av1),
        Some("hevc") | Some("h265") | Some("265") => Some(BitstreamCodec::Hevc),
        _ => None,
    }
}

/// `true` if the input is stdin rather than a path.
pub fn is_stdin(path: &Path) -> bool {
    path == Path::new("-")
}

/// Detect the codec of a file, preferring the extension and falling back to the
/// stream contents. Never fails on unreadable input — that error surfaces
/// later, where it can be reported in context.
pub fn detect_codec(path: &Path) -> BitstreamCodec {
    if let Some(codec) = codec_from_extension(path) {
        return codec;
    }

    let mut head = [0u8; SNIFF_LEN];
    match File::open(path).and_then(|mut f| read_up_to(&mut f, &mut head)) {
        Ok(n) => sniff_codec(&head[..n]),
        Err(_) => BitstreamCodec::Hevc,
    }
}

/// Open an elementary stream and report its codec.
///
/// For stdin the leading bytes are buffered and put back in front of the
/// stream, so the returned reader still yields the complete input.
pub fn open_input(path: &Path) -> Result<(BitstreamCodec, Box<dyn BufRead>)> {
    if is_stdin(path) {
        let mut stdin = std::io::stdin().lock();
        let mut head = vec![0u8; SNIFF_LEN];
        let n = read_up_to(&mut stdin, &mut head)?;
        head.truncate(n);

        let codec = sniff_codec(&head);
        let reader = BufReader::with_capacity(100_000, Cursor::new(head).chain(stdin));

        Ok((codec, Box::new(reader)))
    } else {
        let codec = detect_codec(path);
        let reader = BufReader::with_capacity(100_000, File::open(path)?);

        Ok((codec, Box::new(reader)))
    }
}

/// Read until `buf` is full or the stream ends. Returns the number of bytes read.
fn read_up_to<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;

    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }

    Ok(filled)
}
