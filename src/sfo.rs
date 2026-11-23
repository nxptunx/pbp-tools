// SPDX-License-Identifier: GPL-2.0-only

//! Reader/writer for Sony PSP PARAM.SFO (PSF) files.
//!
//! Header (20 bytes):
//!   0..4   magic 00 50 53 46 ("\\0PSF")
//!   4..8   version 01 01 00 00 (u32 LE = 0x0000_0101)
//!   8..12  offset of key table
//!   12..16 offset of value table
//!   16..20 number of entries
//!
//! Each 16-byte index entry:
//!   0..2   key offset within key table (u16 LE)
//!   2      alignment (normally 4)
//!   3      data type (0 = binary, 2 = UTF-8 string, 4 = u32)
//!   4..8   data length (including NUL for strings)
//!   8..12  data length including padding
//!   12..16 data offset within value table

use crate::pbp::hex;

pub const MAGIC: [u8; 4] = [0x00, b'P', b'S', b'F'];
pub const DEFAULT_VERSION: u32 = 0x0000_0101;
pub const HEADER_SIZE: usize = 20;
pub const INDEX_ENTRY_SIZE: usize = 16;
pub const DEFAULT_ALIGNMENT: u8 = 4;

pub const TYPE_BINARY: u8 = 0;
pub const TYPE_STRING: u8 = 2;
pub const TYPE_INT: u8 = 4;

#[derive(Clone, Debug)]
pub struct SfoEntry {
    pub key: String,
    pub alignment: u8,
    pub data_type: u8,
    /// Exact stored bytes. For strings this normally includes the trailing NUL.
    pub data: Vec<u8>,
    /// Padded size as read from the file, kept for `info --structure`.
    pub raw_padded_len: u32,
    /// Value-table offset as read from the file, kept for `info --structure`.
    pub raw_value_offset: u32,
    /// Key-table offset as read from the file.
    pub raw_key_offset: u16,
}

#[derive(Clone, Debug)]
pub struct SfoFile {
    pub version: u32,
    pub entries: Vec<SfoEntry>,
}

impl SfoFile {
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        if data.len() < HEADER_SIZE {
            return Err(format!(
                "SFO file is too small ({} bytes; need at least {})",
                data.len(),
                HEADER_SIZE
            ));
        }
        if data[0..4] != MAGIC {
            return Err(format!(
                "invalid SFO magic: expected 00 50 53 46 (\\0PSF), got {}",
                hex(&data[0..4])
            ));
        }

        let version = read_u32(data, 4)?;
        let key_table = read_u32(data, 8)? as usize;
        let value_table = read_u32(data, 12)? as usize;
        let count = read_u32(data, 16)? as usize;

        let index_end = HEADER_SIZE
            .checked_add(
                count
                    .checked_mul(INDEX_ENTRY_SIZE)
                    .ok_or("SFO entry count overflow")?,
            )
            .ok_or("SFO entry count overflow")?;
        if index_end > data.len() {
            return Err(format!(
                "invalid SFO: {} entries need {} bytes, file has {}",
                count,
                index_end,
                data.len()
            ));
        }
        if key_table > data.len() || value_table > data.len() {
            return Err("invalid SFO: table offset is past end of file".to_string());
        }
        if key_table < index_end {
            return Err(format!(
                "invalid SFO: key table offset {} overlaps index table (ends at {})",
                key_table, index_end
            ));
        }
        if value_table < key_table {
            return Err(format!(
                "invalid SFO: value table offset {} precedes key table offset {}",
                value_table, key_table
            ));
        }

        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let base = HEADER_SIZE + i * INDEX_ENTRY_SIZE;
            let key_rel = read_u16(data, base)?;
            let alignment = data[base + 2];
            let data_type = data[base + 3];
            let data_len = read_u32(data, base + 4)? as usize;
            let padded_len = read_u32(data, base + 8)?;
            let value_rel = read_u32(data, base + 12)? as usize;

            let key_start = key_table
                .checked_add(key_rel as usize)
                .ok_or("invalid SFO: key offset overflow")?;
            if key_start >= value_table || key_start >= data.len() {
                return Err(format!(
                    "invalid SFO: key for entry {} is outside the key table",
                    i
                ));
            }
            let key_end = data[key_start..value_table]
                .iter()
                .position(|&b| b == 0)
                .map(|p| key_start + p)
                .ok_or_else(|| format!("invalid SFO: key {} is not NUL-terminated", i))?;
            let key = String::from_utf8_lossy(&data[key_start..key_end]).into_owned();

            let value_start = value_table
                .checked_add(value_rel)
                .ok_or("invalid SFO: value offset overflow")?;
            let value_end = value_start
                .checked_add(data_len)
                .ok_or("invalid SFO: value length overflow")?;
            if value_end > data.len() {
                return Err(format!(
                    "invalid SFO: data for '{}' ({}..{}) is outside the file",
                    key, value_start, value_end
                ));
            }
            let padded_end = value_start.saturating_add(padded_len as usize);
            if padded_end > data.len() {
                return Err(format!(
                    "invalid SFO: padded data for '{}' is outside the file",
                    key
                ));
            }

            entries.push(SfoEntry {
                key,
                alignment,
                data_type,
                data: data[value_start..value_end].to_vec(),
                raw_padded_len: padded_len,
                raw_value_offset: value_rel as u32,
                raw_key_offset: key_rel,
            });
        }

        Ok(Self { version, entries })
    }

    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        let data =
            std::fs::read(path).map_err(|e| format!("unable to read {}: {}", path.display(), e))?;
        Self::parse(&data)
    }

    pub fn get(&self, key: &str) -> Option<&SfoEntry> {
        self.entries.iter().find(|e| e.key == key)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut SfoEntry> {
        self.entries.iter_mut().find(|e| e.key == key)
    }

    /// Rebuild the SFO with correct 4-byte alignment and alphabetically sorted keys.
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        let mut entries = self.entries.clone();
        entries.sort_by(|a, b| a.key.as_bytes().cmp(b.key.as_bytes()));

        let count = entries.len();
        let key_table_offset = HEADER_SIZE + count * INDEX_ENTRY_SIZE;
        let mut key_table: Vec<u8> = Vec::new();
        let mut value_table: Vec<u8> = Vec::new();
        let mut index: Vec<u8> = Vec::with_capacity(count * INDEX_ENTRY_SIZE);

        for entry in &entries {
            let key_bytes = entry.key.as_bytes();
            if key_bytes.is_empty() {
                return Err("SFO keys must not be empty".to_string());
            }
            if key_bytes.len() > u16::MAX as usize {
                return Err(format!("SFO key '{}' is too long", entry.key));
            }
            if key_bytes.contains(&0) {
                return Err(format!("SFO key '{}' contains a NUL byte", entry.key));
            }

            let key_rel = key_table.len() as u16;
            key_table.extend_from_slice(key_bytes);
            key_table.push(0);

            let value_rel = u32::try_from(value_table.len())
                .map_err(|_| "SFO value table is too large".to_string())?;
            let data_len = u32::try_from(entry.data.len())
                .map_err(|_| format!("SFO value for '{}' is too large", entry.key))?;
            let alignment = if entry.alignment == 0 {
                DEFAULT_ALIGNMENT
            } else {
                entry.alignment
            };
            let padded = align_to(data_len, alignment)?;

            value_table.extend_from_slice(&entry.data);
            value_table.resize(value_table.len() + (padded - data_len) as usize, 0);

            index.extend_from_slice(&key_rel.to_le_bytes());
            index.push(alignment);
            index.push(entry.data_type);
            index.extend_from_slice(&data_len.to_le_bytes());
            index.extend_from_slice(&padded.to_le_bytes());
            index.extend_from_slice(&value_rel.to_le_bytes());
        }

        while key_table.len() % 4 != 0 {
            key_table.push(0);
        }
        let value_table_offset = key_table_offset + key_table.len();

        let mut out = Vec::with_capacity(value_table_offset + value_table.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&(key_table_offset as u32).to_le_bytes());
        out.extend_from_slice(&(value_table_offset as u32).to_le_bytes());
        out.extend_from_slice(&(count as u32).to_le_bytes());
        out.extend_from_slice(&index);
        out.extend_from_slice(&key_table);
        out.extend_from_slice(&value_table);
        Ok(out)
    }

    pub fn write_file(&self, path: &std::path::Path) -> Result<(), String> {
        let bytes = self.to_bytes()?;
        std::fs::write(path, bytes)
            .map_err(|e| format!("unable to write {}: {}", path.display(), e))
    }
}

impl SfoEntry {
    /// Human-readable value, with the trailing NUL removed for strings.
    pub fn display_value(&self) -> String {
        match self.data_type {
            TYPE_STRING => {
                let bytes = strip_trailing_nul(&self.data);
                String::from_utf8_lossy(bytes).into_owned()
            }
            TYPE_INT => {
                if self.data.len() >= 4 {
                    let arr: [u8; 4] = self.data[..4].try_into().unwrap();
                    u32::from_le_bytes(arr).to_string()
                } else {
                    hex(&self.data)
                }
            }
            _ => hex(&self.data),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self.data_type {
            TYPE_BINARY => "binary",
            TYPE_STRING => "string",
            TYPE_INT => "int",
            _ => "unknown",
        }
    }
}

pub fn new_string_entry(key: &str, text: &str) -> SfoEntry {
    SfoEntry {
        key: key.to_string(),
        alignment: DEFAULT_ALIGNMENT,
        data_type: TYPE_STRING,
        data: encode_string(key, text),
        raw_padded_len: 0,
        raw_value_offset: 0,
        raw_key_offset: 0,
    }
}

pub fn new_int_entry(key: &str, value: u32) -> SfoEntry {
    SfoEntry {
        key: key.to_string(),
        alignment: DEFAULT_ALIGNMENT,
        data_type: TYPE_INT,
        data: value.to_le_bytes().to_vec(),
        raw_padded_len: 0,
        raw_value_offset: 0,
        raw_key_offset: 0,
    }
}

pub fn new_binary_entry(key: &str, data: Vec<u8>) -> SfoEntry {
    SfoEntry {
        key: key.to_string(),
        alignment: DEFAULT_ALIGNMENT,
        data_type: TYPE_BINARY,
        data,
        raw_padded_len: 0,
        raw_value_offset: 0,
        raw_key_offset: 0,
    }
}

/// Decode the classic `<#decimal>` notation, e.g. `<#13><#10>` => CRLF.
pub fn decode_notation(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' && i + 1 < bytes.len() && bytes[i + 1] == b'#' {
            if let Some(rel_end) = bytes[i + 2..].iter().position(|&b| b == b'>') {
                let end = i + 2 + rel_end;
                let digits = &text[i + 2..end];
                if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                    if let Ok(value) = digits.parse::<u16>() {
                        if value <= 255 {
                            out.push(value as u8);
                            i = end + 1;
                            continue;
                        }
                    }
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Encode a string value. Strings are NUL-terminated, except the documented
/// `Category = "MS"` case where the SFO stores exactly two bytes.
pub fn encode_string(key: &str, text: &str) -> Vec<u8> {
    let mut out = decode_notation(text);
    if out.last() != Some(&0) && !(key == "Category" && out == b"MS") {
        out.push(0);
    }
    out
}

/// Parse a CLI value according to an entry's data type.
pub fn value_from_type(entry: &SfoEntry, value: &str) -> Result<Vec<u8>, String> {
    match entry.data_type {
        TYPE_STRING => Ok(encode_string(&entry.key, value)),
        TYPE_INT => {
            let cleaned = value.trim().replace('_', "");
            let number = if let Some(hexpart) = cleaned
                .strip_prefix("0x")
                .or_else(|| cleaned.strip_prefix("0X"))
            {
                u32::from_str_radix(hexpart, 16)
                    .map_err(|e| format!("invalid hexadecimal integer '{}': {}", value, e))?
            } else {
                cleaned
                    .parse::<u32>()
                    .map_err(|e| format!("invalid integer '{}': {}", value, e))?
            };
            Ok(number.to_le_bytes().to_vec())
        }
        TYPE_BINARY => parse_binary(value),
        other => Err(format!(
            "cannot parse a value for unknown SFO data type {}",
            other
        )),
    }
}

/// Parse hex bytes (`41 42`, `0x4142`) or `@path` to read raw bytes from a file.
pub fn parse_binary(value: &str) -> Result<Vec<u8>, String> {
    if let Some(path) = value.strip_prefix('@') {
        return std::fs::read(path)
            .map_err(|e| format!("unable to read binary value from {}: {}", path, e));
    }
    let cleaned: String = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value)
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ',')
        .collect();
    if cleaned.is_empty() {
        return Ok(Vec::new());
    }
    if cleaned.len() % 2 != 0 {
        return Err(format!("hex value '{}' has an odd number of digits", value));
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&cleaned[i..i + 2], 16)
                .map_err(|e| format!("invalid hex value '{}': {}", value, e))
        })
        .collect()
}

pub fn parse_type(s: &str) -> Result<u8, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "0" | "bin" | "binary" => Ok(TYPE_BINARY),
        "2" | "str" | "string" => Ok(TYPE_STRING),
        "4" | "int" | "integer" => Ok(TYPE_INT),
        other => Err(format!(
            "unknown SFO data type '{}'; use 0 (binary), 2 (string) or 4 (int)",
            other
        )),
    }
}

fn strip_trailing_nul(data: &[u8]) -> &[u8] {
    if data.last() == Some(&0) {
        &data[..data.len() - 1]
    } else {
        data
    }
}

fn align_to(value: u32, alignment: u8) -> Result<u32, String> {
    let alignment = if alignment == 0 {
        DEFAULT_ALIGNMENT
    } else {
        alignment
    } as u32;
    let rem = value % alignment;
    if rem == 0 {
        Ok(value)
    } else {
        value
            .checked_add(alignment - rem)
            .ok_or_else(|| "SFO data length overflow".to_string())
    }
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16, String> {
    let bytes: [u8; 2] = data
        .get(offset..offset + 2)
        .ok_or_else(|| format!("unexpected end of SFO file at offset {}", offset))?
        .try_into()
        .map_err(|_| "internal error: bad u16 slice".to_string())?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, String> {
    let bytes: [u8; 4] = data
        .get(offset..offset + 4)
        .ok_or_else(|| format!("unexpected end of SFO file at offset {}", offset))?
        .try_into()
        .map_err(|_| "internal error: bad u32 slice".to_string())?;
    Ok(u32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SfoFile {
        SfoFile {
            version: DEFAULT_VERSION,
            entries: vec![
                new_string_entry("TITLE", "Test Game"),
                new_int_entry("CATEGORY", 4),
                new_string_entry("Category", "MS"),
            ],
        }
    }

    #[test]
    fn roundtrip_and_sort() {
        let sfo = sample();
        let bytes = sfo.to_bytes().unwrap();
        assert_eq!(&bytes[0..4], &MAGIC);
        let parsed = SfoFile::parse(&bytes).unwrap();
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(parsed.entries[0].key, "CATEGORY");
        assert_eq!(parsed.get("TITLE").unwrap().display_value(), "Test Game");
        assert_eq!(parsed.get("CATEGORY").unwrap().display_value(), "4");
        // Category=MS is intentionally not NUL-terminated.
        assert_eq!(parsed.get("Category").unwrap().data, b"MS");
    }

    #[test]
    fn notation_and_alignment() {
        assert_eq!(decode_notation("a<#13><#10>b"), b"a\r\nb");
        let sfo = SfoFile {
            version: DEFAULT_VERSION,
            entries: vec![new_string_entry("DETAIL", "line1<#13><#10>line2")],
        };
        let bytes = sfo.to_bytes().unwrap();
        // 20 header + 16 index + "DETAIL\0"+2 padding + value + padding
        assert_eq!(bytes.len() % 4, 0);
        let parsed = SfoFile::parse(&bytes).unwrap();
        assert_eq!(parsed.get("DETAIL").unwrap().data, b"line1\r\nline2\0");
    }

    #[test]
    fn binary_from_file_syntax() {
        let dir = std::env::temp_dir().join("pbp-rs-sfo-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bin.dat");
        std::fs::write(&path, [1u8, 2, 3]).unwrap();
        let value = format!("@{}", path.display());
        assert_eq!(parse_binary(&value).unwrap(), vec![1, 2, 3]);
        std::fs::remove_file(path).unwrap();
    }
}
