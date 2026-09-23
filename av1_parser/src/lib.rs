#![allow(dead_code)]

use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, ErrorKind, Read, Write};
use std::path::Path;

use anyhow::{Result, bail};
use matroska_demuxer::{MatroskaFile, TrackType};

// ---------------------------------------------------------------------------
// OBU type constants (AV1 spec Table 5)
// ---------------------------------------------------------------------------
pub const OBU_SEQUENCE_HEADER: u8 = 1;
pub const OBU_TEMPORAL_DELIMITER: u8 = 2;
pub const OBU_FRAME_HEADER: u8 = 3;
pub const OBU_METADATA: u8 = 5;
pub const OBU_FRAME: u8 = 6;
pub const OBU_REDUNDANT_FRAME_HEADER: u8 = 7;

/// `trailing_bits()` for an OBU that ends on a byte boundary: one
/// `trailing_one_bit` followed by seven `trailing_zero_bit`s (AV1 spec 5.3.4).
///
/// Every OBU except `OBU_TILE_GROUP`, `OBU_TILE_LIST` and `OBU_FRAME` has to
/// end with them. Metadata payloads such as Dolby Vision RPUs or HDR10+ are
/// byte aligned already, so the trailing bits occupy one extra byte. Strict
/// parsers - FFmpeg's `cbs_av1` among them - reject a unit that lacks it with
/// "trailing_one_bit out of range: 0".
pub const OBU_TRAILING_BITS_BYTE: u8 = 0x80;


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

// ---------------------------------------------------------------------------
// I/O structs
// ---------------------------------------------------------------------------

/// Iterates OBUs from a raw AV1 byte stream.
pub struct ObuReader<R: Read> {
    reader: R,
}

impl<R: Read> ObuReader<R> {
    pub fn new(reader: R) -> Self {
        ObuReader { reader }
    }
    pub fn next_obu(&mut self) -> Result<Option<Obu>> {
        Obu::read_from(&mut self.reader)
    }
    pub fn into_inner(self) -> R {
        self.reader
    }
}

impl<R: Read> Iterator for ObuReader<R> {
    type Item = Result<Obu>;
    fn next(&mut self) -> Option<Self::Item> {
        self.next_obu().transpose()
    }
}

/// Writes IVF frames. Writes the file header in `new()`.
pub struct IvfWriter<W: Write> {
    writer: W,
}

impl<W: Write> IvfWriter<W> {
    /// Writes the 32-byte IVF file header immediately.
    pub fn new(mut writer: W, file_header: &[u8; 32]) -> Result<Self> {
        writer.write_all(file_header)?;
        Ok(IvfWriter { writer })
    }

    /// Writes one IVF frame (12-byte frame header + frame data).
    pub fn write_frame(&mut self, timestamp: u64, frame_data: &[u8]) -> Result<()> {
        write_ivf_frame_header(&mut self.writer, frame_data.len() as u32, timestamp)?;
        self.writer.write_all(frame_data)?;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush().map_err(Into::into)
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

/// Writes raw AV1 OBUs directly.
pub struct ObuWriter<W: Write> {
    writer: W,
}

impl<W: Write> ObuWriter<W> {
    pub fn new(writer: W) -> Self {
        ObuWriter { writer }
    }
    pub fn write_raw(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes).map_err(Into::into)
    }
    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush().map_err(Into::into)
    }
    pub fn into_inner(self) -> W {
        self.writer
    }
}

// ---------------------------------------------------------------------------
// Metadata placement
// ---------------------------------------------------------------------------

/// Position within a temporal unit at which per-frame metadata OBUs belong:
/// immediately before the first frame or frame header OBU, therefore *after*
/// the temporal delimiter, the sequence header and any static metadata.
///
/// Inserting earlier — right behind the temporal delimiter — makes muxers treat
/// the metadata as part of the AV1 codec configuration record, where per-frame
/// metadata has no business being.
///
/// Returns `obus.len()` when the temporal unit carries no frame at all.
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
/// delimiter or sequence header in low overhead format). Everything else —
/// HEVC Annex B start codes included — is reported as HEVC, which keeps the
/// previous behaviour for anything unexpected.
pub fn sniff_codec(buf: &[u8]) -> BitstreamCodec {
    if buf.len() >= 4 && buf[..4] == IVF_SIGNATURE {
        return BitstreamCodec::Av1;
    }

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

/// `true` if the input refers to stdin rather than a path.
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
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }

    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obu(obu_type: u8) -> Obu {
        Obu {
            obu_type,
            temporal_id: 0,
            spatial_id: 0,
            payload: Vec::new(),
            raw_bytes: Vec::new(),
        }
    }

    #[test]
    fn insert_index_skips_sequence_header_and_static_metadata() {
        let tu = [
            obu(OBU_TEMPORAL_DELIMITER),
            obu(OBU_SEQUENCE_HEADER),
            obu(OBU_METADATA),
            obu(OBU_FRAME),
        ];
        assert_eq!(metadata_insert_index(&tu), 3);

        let tu = [obu(OBU_TEMPORAL_DELIMITER), obu(OBU_FRAME)];
        assert_eq!(metadata_insert_index(&tu), 1);

        let tu = [obu(OBU_TEMPORAL_DELIMITER), obu(OBU_FRAME_HEADER)];
        assert_eq!(metadata_insert_index(&tu), 1);

        // No frame in this temporal unit
        let tu = [obu(OBU_TEMPORAL_DELIMITER)];
        assert_eq!(metadata_insert_index(&tu), 1);
    }

    #[test]
    fn sniffing_tells_the_two_bitstreams_apart() {
        assert_eq!(sniff_codec(b"DKIF\0\0 \0"), BitstreamCodec::Av1);

        // Temporal delimiter OBU, then sequence header OBU
        assert_eq!(sniff_codec(&[0x12, 0x00]), BitstreamCodec::Av1);
        assert_eq!(sniff_codec(&[0x0A, 0x0F]), BitstreamCodec::Av1);

        // HEVC Annex B, three and four byte start codes
        assert_eq!(sniff_codec(&[0x00, 0x00, 0x01, 0x40]), BitstreamCodec::Hevc);
        assert_eq!(
            sniff_codec(&[0x00, 0x00, 0x00, 0x01, 0x40]),
            BitstreamCodec::Hevc
        );

        // Matroska and anything unrecognised stay on the HEVC path
        assert_eq!(sniff_codec(&[0x1A, 0x45, 0xDF, 0xA3]), BitstreamCodec::Hevc);
        assert_eq!(sniff_codec(&[]), BitstreamCodec::Hevc);
    }

    #[test]
    fn extension_detection_is_case_insensitive() {
        assert_eq!(
            codec_from_extension(Path::new("a.AV1")),
            Some(BitstreamCodec::Av1)
        );
        assert_eq!(
            codec_from_extension(Path::new("a.ivf")),
            Some(BitstreamCodec::Av1)
        );
        assert_eq!(
            codec_from_extension(Path::new("a.H265")),
            Some(BitstreamCodec::Hevc)
        );
        assert_eq!(codec_from_extension(Path::new("a.mkv")), None);
        assert_eq!(codec_from_extension(Path::new("-")), None);
    }
}

// ---------------------------------------------------------------------------
// AV1 in Matroska
// ---------------------------------------------------------------------------

/// Matroska codec id of an AV1 video track.
pub const MATROSKA_AV1_CODEC_ID: &str = "V_AV1";

/// Matroska codec id of an HEVC video track.
pub const MATROSKA_HEVC_CODEC_ID: &str = "V_MPEGH/ISO/HEVC";

/// A temporal delimiter OBU with an empty payload, the way every temporal unit
/// of a low overhead bitstream starts.
pub fn temporal_delimiter_obu() -> Obu {
    Obu {
        obu_type: OBU_TEMPORAL_DELIMITER,
        temporal_id: 0,
        spatial_id: 0,
        payload: Vec::new(),
        raw_bytes: vec![(OBU_TEMPORAL_DELIMITER << 3) | 0x02, 0x00],
    }
}

/// Codec of the first video track in a Matroska file, or `None` if the file is
/// not Matroska or carries neither AV1 nor HEVC video.
pub fn matroska_video_codec(path: &Path) -> Option<BitstreamCodec> {
    let file = File::open(path).ok()?;
    let mkv = MatroskaFile::open(file).ok()?;

    mkv.tracks()
        .iter()
        .filter(|t| t.track_type() == TrackType::Video)
        .find_map(|t| match t.codec_id() {
            MATROSKA_AV1_CODEC_ID => Some(BitstreamCodec::Av1),
            MATROSKA_HEVC_CODEC_ID => Some(BitstreamCodec::Hevc),
            _ => None,
        })
}

/// Reads the AV1 video track of a Matroska file, one temporal unit at a time.
///
/// A Matroska block holds exactly the OBUs of one temporal unit, so no
/// framing is needed beyond parsing the OBUs out of the block.
pub struct MatroskaAv1Reader {
    mkv: MatroskaFile<File>,
    track_id: u64,
    frame: matroska_demuxer::Frame,
}

impl MatroskaAv1Reader {
    pub fn open(path: &Path) -> Result<Self> {
        let mkv = MatroskaFile::open(File::open(path)?)?;

        let track = mkv
            .tracks()
            .iter()
            .find(|t| t.track_type() == TrackType::Video && t.codec_id() == MATROSKA_AV1_CODEC_ID);

        let Some(track) = track else {
            bail!("No AV1 video track found in file");
        };
        let track_id = track.track_number().get();

        Ok(Self {
            mkv,
            track_id,
            frame: matroska_demuxer::Frame::default(),
        })
    }

    /// Next temporal unit of the AV1 track, or `None` at the end of the file.
    pub fn next_temporal_unit(&mut self) -> Result<Option<Vec<Obu>>> {
        loop {
            if !self.mkv.next_frame(&mut self.frame)? {
                return Ok(None);
            }

            if self.frame.track != self.track_id {
                continue;
            }

            let mut obus = read_obus_from_ivf_frame(std::mem::take(&mut self.frame.data))?;

            // Matroska stores a temporal unit without its delimiter, so put it
            // back. Without it a raw stream written from these OBUs has no
            // temporal unit boundaries left and collapses into a single one.
            if obus.first().map(|o| o.obu_type) != Some(OBU_TEMPORAL_DELIMITER) {
                obus.insert(0, temporal_delimiter_obu());
            }

            return Ok(Some(obus));
        }
    }
}

#[cfg(test)]
mod matroska_tests {
    use super::*;

    #[test]
    fn temporal_delimiter_is_two_bytes() {
        let td = temporal_delimiter_obu();

        assert_eq!(td.raw_bytes, [0x12, 0x00]);
        assert_eq!(td.obu_type, OBU_TEMPORAL_DELIMITER);

        // and it reads back as the same thing
        let mut cursor = std::io::Cursor::new(td.raw_bytes.clone());
        let parsed = Obu::read_from(&mut cursor).unwrap().unwrap();

        assert_eq!(parsed.obu_type, OBU_TEMPORAL_DELIMITER);
        assert!(parsed.payload.is_empty());
        assert_eq!(cursor.position() as usize, td.raw_bytes.len());
    }
}
