use crate::{domain::Digest, error::AppResult};
use serde::{Deserialize, Serialize};
use std::io::{self, BufReader, Read};

pub const JPEG_FINGERPRINT_VERSION: &str = "jpeg-content-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JpegInspection {
    pub digest: Digest,
    pub size: u64,
    pub image: Option<Digest>,
}
struct Tracked<R> {
    input: R,
    full: blake3::Hasher,
    size: u64,
}
impl<R: Read> Read for Tracked<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.input.read(bytes)?;
        self.size = self
            .size
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("JPEG size overflow"))?;
        self.full.update(&bytes[..count]);
        crate::metrics::hash_bytes(count as u64);
        Ok(count)
    }
}
pub fn inspect_jpeg(input: impl Read) -> AppResult<JpegInspection> {
    let _timing = crate::metrics::Timer::start(crate::metrics::Stage::Hash);
    let mut reader = Tracked {
        input: BufReader::new(input),
        full: blake3::Hasher::new(),
        size: 0,
    };
    let image = match fingerprint(&mut reader) {
        Ok(image) => image,
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => None,
        Err(error) => return Err(error.into()),
    };
    io::copy(&mut reader, &mut io::sink())?;
    Ok(JpegInspection {
        digest: Digest::parse(&format!("blake3:{}", reader.full.finalize().to_hex()))?,
        size: reader.size,
        image: image
            .map(|hash| Digest::parse(&format!("blake3:{}", hash.finalize().to_hex())))
            .transpose()?,
    })
}
pub fn inspect_jpeg_source(entry: &super::SourceEntry) -> AppResult<JpegInspection> {
    entry.validate()?;
    let file = entry
        .root
        .open_relative(&entry.path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
    let result = inspect_jpeg(file)?;
    entry.validate()?;
    if result.size != entry.size {
        return Err(crate::error::AppError::integrity(
            "JPEG source size changed",
        ));
    }
    Ok(result)
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rendering {
    orientation: u16,
    colour_space: Option<u16>,
    gamma: Option<(u32, u32)>,
    extra: Vec<(u32, u16, u32, Vec<u8>)>,
}
impl Default for Rendering {
    fn default() -> Self {
        Self {
            orientation: 1,
            colour_space: None,
            gamma: None,
            extra: Vec::new(),
        }
    }
}
fn fingerprint(reader: &mut impl Read) -> io::Result<Option<blake3::Hasher>> {
    let mut soi = [0; 2];
    reader.read_exact(&mut soi)?;
    if soi != [0xff, 0xd8] {
        return Ok(None);
    }
    let mut hash = blake3::Hasher::new_derive_key("photo-sorter JPEG image content v1");
    hash.update(&soi);
    let mut rendering = None;
    let mut xmp = std::collections::BTreeSet::new();
    let mut xmp_bytes = 0usize;
    let (mut frame, mut quantization, mut huffman) = (false, false, false);
    loop {
        let mut header = [0; 4];
        reader.read_exact(&mut header)?;
        if header[0] != 0xff || matches!(header[1], 0 | 0xff | 0x01 | 0xd0..=0xd9) {
            return Ok(None);
        }
        let length = u16::from_be_bytes([header[2], header[3]]) as usize;
        if length < 2 {
            return Ok(None);
        }
        let mut payload = vec![0; length - 2];
        reader.read_exact(&mut payload)?;
        let marker = header[1];
        match marker {
            0xe1 if payload.starts_with(b"Exif\0\0") => {
                let Some(value) = exif_rendering(&payload[6..]) else {
                    return Ok(None);
                };
                if rendering
                    .as_ref()
                    .is_some_and(|previous| previous != &value)
                {
                    return Ok(None);
                }
                rendering = Some(value);
                continue;
            }
            0xe1 if payload.starts_with(b"http://ns.adobe.com/xmp/extension/\0") => {
                return Ok(None);
            }
            0xe1 if payload.starts_with(b"http://ns.adobe.com/xap/1.0/\0") => {
                let Some(properties) = super::jpeg_xmp::properties(&payload[29..]) else {
                    return Ok(None);
                };
                xmp_bytes = xmp_bytes.saturating_add(payload.len());
                if xmp_bytes > 1024 * 1024 {
                    return Ok(None);
                }
                xmp.extend(properties);
                continue;
            }
            0xfe => continue,
            0xed if payload.starts_with(b"Photoshop 3.0\0") => {
                let Some(normalized) = photoshop_without_iptc(&payload) else {
                    return Ok(None);
                };
                if !normalized.is_empty() {
                    hash.update(&[0xff, marker]);
                    hash.update(&((normalized.len() + 2) as u16).to_be_bytes());
                    hash.update(&normalized);
                }
                continue;
            }
            0xc0 => {
                if frame
                    || payload.len() < 6
                    || payload[0] != 8
                    || payload[1..3] == [0, 0]
                    || payload[3..5] == [0, 0]
                    || !matches!(payload[5], 1 | 3 | 4)
                    || payload.len() != 6 + 3 * payload[5] as usize
                {
                    return Ok(None);
                }
                frame = true;
            }
            0xc1..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf | 0xdc | 0xde | 0xdf => {
                return Ok(None);
            }
            0xdb => quantization = true,
            0xc4 => huffman = true,
            _ => {}
        }
        hash.update(&header);
        hash.update(&payload);
        if marker == 0xda {
            if !frame
                || !quantization
                || !huffman
                || payload.len() < 6
                || payload.len() != 4 + 2 * payload[0] as usize
            {
                return Ok(None);
            }
            let (mut after_ff, mut ended) = (false, false);
            let mut buffer = [0; 128 * 1024];
            loop {
                let count = reader.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
                for byte in &buffer[..count] {
                    if ended {
                        continue;
                    }
                    if after_ff {
                        match byte {
                            0 | 0xd0..=0xd7 => after_ff = false,
                            0xff => {}
                            0xd9 => ended = true,
                            _ => return Ok(None),
                        }
                    } else {
                        after_ff = *byte == 0xff;
                    }
                }
            }
            if !ended {
                return Ok(None);
            }
            let rendering = rendering.unwrap_or_default();
            hash.update(&rendering.orientation.to_le_bytes());
            if let Some(colour) = rendering.colour_space {
                hash.update(&[1]);
                hash.update(&colour.to_le_bytes());
            } else {
                hash.update(&[0]);
            }
            if let Some((numerator, denominator)) = rendering.gamma {
                hash.update(&[1]);
                hash.update(&numerator.to_le_bytes());
                hash.update(&denominator.to_le_bytes());
            } else {
                hash.update(&[0]);
            }
            hash.update(&serde_json::to_vec(&rendering.extra).map_err(io::Error::other)?);
            xmp.retain(|property| {
                !property.is_redundant_rendering(rendering.orientation, rendering.colour_space)
            });
            hash.update(&serde_json::to_vec(&xmp).map_err(io::Error::other)?);
            return Ok(Some(hash));
        }
    }
}
struct Tiff<'a> {
    bytes: &'a [u8],
    little: bool,
}
impl Tiff<'_> {
    fn u16(&self, offset: usize) -> Option<u16> {
        let bytes: [u8; 2] = self
            .bytes
            .get(offset..offset.checked_add(2)?)?
            .try_into()
            .ok()?;
        Some(if self.little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    }
    fn u32(&self, offset: usize) -> Option<u32> {
        let bytes: [u8; 4] = self
            .bytes
            .get(offset..offset.checked_add(4)?)?
            .try_into()
            .ok()?;
        Some(if self.little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    }
    fn directory(&self, offset: usize) -> Option<std::collections::BTreeMap<u16, usize>> {
        if offset < 8 {
            return None;
        }
        let count = self.u16(offset)? as usize;
        self.bytes
            .get(offset..offset.checked_add(2 + 12 * count + 4)?)?;
        let mut result = std::collections::BTreeMap::new();
        for index in 0..count {
            let entry = offset + 2 + 12 * index;
            if result.insert(self.u16(entry)?, entry).is_some() {
                return None;
            }
        }
        Some(result)
    }
    fn normalized_field(&self, entry: usize) -> Option<(u16, u32, Vec<u8>)> {
        let kind = self.u16(entry + 2)?;
        let count = self.u32(entry + 4)?;
        let (size, unit) = match kind {
            1 | 2 | 6 | 7 => (1, 1),
            3 | 8 => (2, 2),
            4 | 9 | 11 => (4, 4),
            5 | 10 => (8, 4),
            12 => (8, 8),
            _ => return None,
        };
        let length = (count as usize).checked_mul(size)?;
        let start = if length <= 4 {
            entry + 8
        } else {
            self.u32(entry + 8)? as usize
        };
        let bytes = self.bytes.get(start..start.checked_add(length)?)?;
        let mut result = Vec::with_capacity(length);
        for chunk in bytes.chunks_exact(unit) {
            if self.little || unit == 1 {
                result.extend_from_slice(chunk);
            } else {
                result.extend(chunk.iter().rev());
            }
        }
        Some((kind, count, result))
    }
    fn short(&self, entry: usize) -> Option<u16> {
        if self.u16(entry + 2)? != 3 || self.u32(entry + 4)? != 1 {
            return None;
        }
        self.u16(entry + 8)
    }
}
fn exif_rendering(bytes: &[u8]) -> Option<Rendering> {
    let little = match bytes.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let tiff = Tiff { bytes, little };
    if tiff.u16(2)? != 42 {
        return None;
    }
    let first = tiff.directory(tiff.u32(4)? as usize)?;
    let mut result = Rendering::default();
    for tag in [
        0x0106, 0x012d, 0x013e, 0x013f, 0x0211, 0x0212, 0x0213, 0x0214, 0x8773,
    ] {
        if let Some(entry) = first.get(&tag) {
            let (kind, count, value) = tiff.normalized_field(*entry)?;
            result.extra.push((tag as u32, kind, count, value));
        }
    }
    if let Some(entry) = first.get(&0x112) {
        result.orientation = tiff.short(*entry)?;
        if !(1..=8).contains(&result.orientation) {
            return None;
        }
    }
    if let Some(entry) = first.get(&0x8769) {
        if tiff.u16(entry + 2)? != 4 || tiff.u32(entry + 4)? != 1 {
            return None;
        }
        let exif = tiff.directory(tiff.u32(entry + 8)? as usize)?;
        if let Some(entry) = exif.get(&0xa005) {
            if tiff.u16(entry + 2)? != 4 || tiff.u32(entry + 4)? != 1 {
                return None;
            }
            let interoperability = tiff.directory(tiff.u32(entry + 8)? as usize)?;
            for tag in [1, 2] {
                if let Some(entry) = interoperability.get(&tag) {
                    let (kind, count, value) = tiff.normalized_field(*entry)?;
                    result
                        .extra
                        .push((0x20000 + tag as u32, kind, count, value));
                }
            }
        }
        if let Some(entry) = exif.get(&0xa001) {
            result.colour_space = Some(tiff.short(*entry)?);
        }
        if let Some(entry) = exif.get(&0xa500) {
            if tiff.u16(entry + 2)? != 5 || tiff.u32(entry + 4)? != 1 {
                return None;
            }
            let offset = tiff.u32(entry + 8)? as usize;
            let denominator = tiff.u32(offset + 4)?;
            if denominator == 0 {
                return None;
            }
            result.gamma = Some((tiff.u32(offset)?, denominator));
        }
    }
    Some(result)
}
fn photoshop_without_iptc(payload: &[u8]) -> Option<Vec<u8>> {
    let mut position = 14;
    let mut result = b"Photoshop 3.0\0".to_vec();
    while position < payload.len() {
        let start = position;
        if payload.get(position..position + 4)? != b"8BIM" {
            return None;
        }
        let id = u16::from_be_bytes(payload.get(position + 4..position + 6)?.try_into().ok()?);
        position += 6;
        let name = *payload.get(position)? as usize;
        position = position.checked_add((name + 2) & !1)?;
        let size =
            u32::from_be_bytes(payload.get(position..position + 4)?.try_into().ok()?) as usize;
        position = position
            .checked_add(4)?
            .checked_add(size)?
            .checked_add(size % 2)?;
        let block = payload.get(start..position)?;
        if !matches!(id, 0x0404 | 0x0425) {
            result.extend_from_slice(block);
        }
    }
    if result.len() == 14 {
        result.clear();
    }
    Some(result)
}
