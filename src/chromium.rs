use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

// the localStorage one origin kept in a Chromium profile's `Local Storage/leveldb`, as Electron
// and WebView2 apps leave it; a missing folder holds nothing, and a damaged file or record is
// skipped as LevelDB's own recovery would drop it
pub(crate) fn local_storage(directory: &Path, origin: &str) -> io::Result<Vec<(String, String)>> {
    let files = match fs::read_dir(directory) {
        Ok(files) => files,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut latest = Latest::default();
    for file in files {
        let path = file?.path();
        match path.extension().and_then(|extension| extension.to_str()) {
            Some("log") => {
                for batch in log_records(&fs::read(&path)?) {
                    latest.apply_batch(&batch);
                }
            }
            Some("ldb" | "sst") => {
                latest.read_table(&fs::read(&path)?);
            }
            _ => {}
        }
    }
    let prefix = format!("_{origin}\0").into_bytes();
    Ok(latest
        .0
        .into_iter()
        .filter_map(|(key, (_, value))| {
            Some((
                decode(key.strip_prefix(prefix.as_slice())?)?,
                decode(&value?)?,
            ))
        })
        .collect())
}

// each key at its highest sequence number, with None where that write deleted it
#[derive(Default)]
struct Latest(BTreeMap<Vec<u8>, (u64, Option<Vec<u8>>)>);

impl Latest {
    fn keep(&mut self, key: &[u8], sequence: u64, value: Option<&[u8]>) {
        if self.0.get(key).is_none_or(|(kept, _)| *kept < sequence) {
            self.0
                .insert(key.to_vec(), (sequence, value.map(<[u8]>::to_vec)));
        }
    }

    // a write batch: its first sequence number, a count, then puts and deletes
    fn apply_batch(&mut self, batch: &[u8]) -> Option<()> {
        let sequence = u64::from_le_bytes(batch.get(..8)?.try_into().ok()?);
        let count = u32::from_le_bytes(batch.get(8..12)?.try_into().ok()?);
        let mut rest = &batch[12..];
        for index in 0..u64::from(count) {
            let (&tag, tail) = rest.split_first()?;
            rest = tail;
            let key = slice(&mut rest)?;
            let value = match tag {
                1 => Some(slice(&mut rest)?),
                0 => None,
                _ => return None,
            };
            self.keep(key, sequence + index, value);
        }
        Some(())
    }

    fn read_table(&mut self, bytes: &[u8]) -> Option<()> {
        const MAGIC: u64 = 0xdb47_7524_8b80_fb57;
        let footer = bytes.get(bytes.len().checked_sub(48)?..)?;
        if footer[40..] != MAGIC.to_le_bytes() {
            return None;
        }
        let mut handles = footer;
        let _metaindex = (varint(&mut handles)?, varint(&mut handles)?);
        let index = block(bytes, &mut handles)?;
        for (_, handle) in entries(&index)? {
            let data = block(bytes, &mut handle.as_slice())?;
            for (key, value) in entries(&data)? {
                let (user, trailer) = key.split_at_checked(key.len().checked_sub(8)?)?;
                let trailer = u64::from_le_bytes(trailer.try_into().ok()?);
                let value = match trailer & 0xff {
                    1 => Some(value.as_slice()),
                    0 => None,
                    _ => continue,
                };
                self.keep(user, trailer >> 8, value);
            }
        }
        Some(())
    }
}

// the records of a log file, joined across its 32 KiB blocks; reading stops at a torn or
// damaged record, where a crash left the log
fn log_records(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut records = Vec::new();
    let mut pending = Vec::new();
    for block in bytes.chunks(32 * 1024) {
        let mut at = 0;
        while let Some(header) = block.get(at..at + 7) {
            let length = usize::from(u16::from_le_bytes([header[4], header[5]]));
            let kind = header[6];
            let Some(data) = block.get(at + 7..at + 7 + length) else {
                return records;
            };
            let checksum = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
            if kind == 0 {
                break;
            }
            if unmask(checksum) != crc32c(&[&[kind], data]) {
                return records;
            }
            at += 7 + length;
            match kind {
                1 => records.push(data.to_vec()),
                2 => pending = data.to_vec(),
                3 => pending.extend_from_slice(data),
                4 => {
                    pending.extend_from_slice(data);
                    records.push(std::mem::take(&mut pending));
                }
                _ => return records,
            }
        }
    }
    records
}

// the block a handle points at, checked and decompressed
fn block(bytes: &[u8], handle: &mut &[u8]) -> Option<Vec<u8>> {
    let start = usize::try_from(varint(handle)?).ok()?;
    let end = start.checked_add(usize::try_from(varint(handle)?).ok()?)?;
    let contents = bytes.get(start..end)?;
    let trailer = bytes.get(end..end.checked_add(5)?)?;
    let checksum = u32::from_le_bytes(trailer[1..].try_into().ok()?);
    if unmask(checksum) != crc32c(&[contents, &trailer[..1]]) {
        return None;
    }
    match trailer[0] {
        0 => Some(contents.to_vec()),
        1 => snappy(contents),
        _ => None,
    }
}

// a block's prefix-compressed entries, before its restart array
fn entries(block: &[u8]) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
    let count = block.len().checked_sub(4)?;
    let restarts = usize::try_from(u32::from_le_bytes(block[count..].try_into().ok()?)).ok()?;
    let mut rest = block.get(..count.checked_sub(restarts.checked_mul(4)?)?)?;
    let mut key = Vec::new();
    let mut found = Vec::new();
    while !rest.is_empty() {
        let shared = usize::try_from(varint(&mut rest)?).ok()?;
        let unshared = usize::try_from(varint(&mut rest)?).ok()?;
        let length = usize::try_from(varint(&mut rest)?).ok()?;
        if shared > key.len() {
            return None;
        }
        key.truncate(shared);
        let (delta, tail) = rest.split_at_checked(unshared)?;
        key.extend_from_slice(delta);
        let (value, tail) = tail.split_at_checked(length)?;
        rest = tail;
        found.push((key.clone(), value.to_vec()));
    }
    Some(found)
}

fn varint(rest: &mut &[u8]) -> Option<u64> {
    let mut value = 0_u64;
    for shift in (0..64).step_by(7) {
        let (&byte, tail) = rest.split_first()?;
        *rest = tail;
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Some(value);
        }
    }
    None
}

fn slice<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
    let length = usize::try_from(varint(rest)?).ok()?;
    let (head, tail) = rest.split_at_checked(length)?;
    *rest = tail;
    Some(head)
}

// raw snappy, as LevelDB compresses table blocks
fn snappy(input: &[u8]) -> Option<Vec<u8>> {
    let mut rest = input;
    let length = usize::try_from(varint(&mut rest)?).ok()?;
    let mut out = Vec::with_capacity(length.min(1 << 24));
    while let Some((&tag, tail)) = rest.split_first() {
        rest = tail;
        let (copied, offset) = match tag & 3 {
            0 => {
                let mut literal = usize::from(tag >> 2);
                if literal >= 60 {
                    let (bytes, tail) = rest.split_at_checked(literal - 59)?;
                    rest = tail;
                    literal = bytes
                        .iter()
                        .rev()
                        .fold(0, |sum, &byte| sum << 8 | usize::from(byte));
                }
                let (bytes, tail) = rest.split_at_checked(literal.checked_add(1)?)?;
                rest = tail;
                out.extend_from_slice(bytes);
                continue;
            }
            1 => {
                let (&low, tail) = rest.split_first()?;
                rest = tail;
                (
                    4 + usize::from(tag >> 2 & 7),
                    usize::from(tag >> 5) << 8 | usize::from(low),
                )
            }
            2 => {
                let (bytes, tail) = rest.split_at_checked(2)?;
                rest = tail;
                (
                    usize::from(tag >> 2) + 1,
                    usize::from(u16::from_le_bytes([bytes[0], bytes[1]])),
                )
            }
            _ => {
                let (bytes, tail) = rest.split_at_checked(4)?;
                rest = tail;
                (
                    usize::from(tag >> 2) + 1,
                    usize::try_from(u32::from_le_bytes(bytes.try_into().ok()?)).ok()?,
                )
            }
        };
        let start = out.len().checked_sub(offset).filter(|_| offset > 0)?;
        if out.len() + copied > length {
            return None;
        }
        // a copy may overlap what it writes, repeating a short run
        for index in start..start + copied {
            out.push(out[index]);
        }
    }
    (out.len() == length).then_some(out)
}

fn crc32c(parts: &[&[u8]]) -> u32 {
    let mut crc = !0_u32;
    for &byte in parts.iter().flat_map(|part| part.iter()) {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0x82f6_3b78 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

// leveldb stores checksums rotated and offset, so a checksum of checksums stays distinct
fn unmask(masked: u32) -> u32 {
    masked.wrapping_sub(0xa282_ead8).rotate_left(15)
}

// a format byte, then UTF-16LE (0) or Latin-1 (1)
fn decode(bytes: &[u8]) -> Option<String> {
    match bytes.split_first()? {
        (0, text) => {
            let (units, []) = text.as_chunks::<2>() else {
                return None;
            };
            Some(String::from_utf16_lossy(
                &units
                    .iter()
                    .map(|&unit| u16::from_le_bytes(unit))
                    .collect::<Vec<_>>(),
            ))
        }
        (1, text) => Some(text.iter().map(|&byte| char::from(byte)).collect()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(crc: u32) -> u32 {
        crc.rotate_right(15).wrapping_add(0xa282_ead8)
    }

    fn batch(sequence: u64, writes: &[(&[u8], Option<&[u8]>)]) -> Vec<u8> {
        let mut bytes = sequence.to_le_bytes().to_vec();
        bytes.extend((writes.len() as u32).to_le_bytes());
        for (key, value) in writes {
            bytes.push(u8::from(value.is_some()));
            bytes.push(key.len() as u8);
            bytes.extend(*key);
            if let Some(value) = value {
                bytes.push(value.len() as u8);
                bytes.extend(*value);
            }
        }
        bytes
    }

    fn record(kind: u8, data: &[u8]) -> Vec<u8> {
        let mut bytes = mask(crc32c(&[&[kind], data])).to_le_bytes().to_vec();
        bytes.extend((data.len() as u16).to_le_bytes());
        bytes.push(kind);
        bytes.extend(data);
        bytes
    }

    #[test]
    fn checksums_match_the_castagnoli_check_value() {
        assert_eq!(crc32c(&[b"1234", b"56789"]), 0xe306_9283);
        assert_eq!(unmask(mask(0x1234_5678)), 0x1234_5678);
    }

    #[test]
    fn snappy_expands_literals_and_overlapping_copies() {
        // "abcd" then a 1-byte-offset copy of 8 from 4 back, then a 2-byte-offset copy of 3
        let stream = [
            15,
            3 << 2,
            b'a',
            b'b',
            b'c',
            b'd',
            1 | (4 << 2),
            4,
            2 | (2 << 2),
            12,
            0,
        ];
        assert_eq!(snappy(&stream).as_deref(), Some(&b"abcdabcdabcdabc"[..]));
        assert_eq!(snappy(&[4, 0, b'a']), None);
    }

    #[test]
    fn the_latest_write_to_each_key_wins_and_deletes_drop_it() {
        let dir = std::env::temp_dir().join(format!("ceri-chromium-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let key = |name: &str| [b"_http://tauri.localhost\0\x01", name.as_bytes()].concat();
        let (view, gone, other) = (
            key("view"),
            key("gone"),
            b"_https://other\0\x01view".to_vec(),
        );
        let utf16: Vec<u8> = std::iter::once(0)
            .chain(
                "caf\u{e9} \u{1f3ac}"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        let first = batch(
            1,
            &[
                (&view, Some(b"\x01old")),
                (&gone, Some(b"\x01x")),
                (&other, Some(b"\x01no")),
            ],
        );
        let second = batch(4, &[(&gone, None), (&view, Some(&utf16))]);
        // the second batch spans two records, as a write across a block edge does
        let (head, tail) = second.split_at(10);
        let log = [
            record(1, &first),
            record(2, head),
            record(4, tail),
            vec![0; 7],
        ]
        .concat();
        fs::write(dir.join("000003.log"), log).unwrap();
        let found = local_storage(&dir, "http://tauri.localhost").unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            found,
            [("view".to_string(), "caf\u{e9} \u{1f3ac}".to_string())]
        );
        assert!(
            local_storage(&dir, "http://tauri.localhost")
                .unwrap()
                .is_empty()
        );
    }
}
