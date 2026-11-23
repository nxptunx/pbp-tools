// SPDX-License-Identifier: GPL-2.0-only

//! Reader/writer for Sony PSP PBP archives.
//!
//! Format:
//!   0..4   magic: 00 50 42 50 ("\\0PBP")
//!   4..8   version: 00 00 01 00 (u32 LE = 0x0001_0000)
//!   8..40  eight u32 LE offsets, one per fixed slot
//!   40..   file data, concatenated with no separators
//!
//! An absent slot has the same offset as the previous *present* slot.

use std::fs;
use std::path::Path;

pub const SLOT_NAMES: [&str; 8] = [
    "PARAM.SFO",
    "ICON0.PNG",
    "ICON1.PMF",
    "PIC0.PNG",
    "PIC1.PNG",
    "SND0.AT3",
    "DATA.PSP",
    "DATA.PSAR",
];

/// Normal PSP archives start with `\0PBP`. You still run into `\0PSP` in
/// older notes now and then, so we accept that when reading too. Harmless.
pub const MAGIC: [u8; 4] = [0x00, b'P', b'B', b'P'];
pub const LEGACY_MAGIC: [u8; 4] = [0x00, b'P', b'S', b'P'];
pub const DEFAULT_VERSION: u32 = 0x0001_0000;
pub const HEADER_SIZE: usize = 40;

#[derive(Clone, Debug)]
pub struct PbpFile {
    pub slot: usize,
    pub name: &'static str,
    pub offset: u32,
    pub size: usize,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct PbpArchive {
    /// Magic bytes as read from the file (`\0PBP`, or legacy `\0PSP`).
    pub magic: [u8; 4],
    pub version: u32,
    /// Raw offsets exactly as found in the header (useful for `info`).
    pub offsets: [u32; 8],
    pub files: [Option<PbpFile>; 8],
}

impl PbpArchive {
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        if data.len() < HEADER_SIZE {
            return Err(format!(
                "PBP file is too small ({} bytes; need at least {})",
                data.len(),
                HEADER_SIZE
            ));
        }
        let magic: [u8; 4] = data[0..4].try_into().unwrap();
        if magic != MAGIC && magic != LEGACY_MAGIC {
            let hint = if magic == *b"PSAR" {
                " - this looks like an extracted DATA.PSAR member; pass the .PBP file to `extract`"
            } else if magic == *b"\0PSF" {
                " - this is a PARAM.SFO; use `pbp sfo ...` instead"
            } else if magic == [0x89, b'P', b'N', b'G'] {
                " - this is a PNG member; use an image viewer"
            } else {
                ""
            };
            return Err(format!(
                "invalid PBP magic: expected 00 50 42 50 (\\0PBP), got {}{}",
                hex(&magic),
                hint
            ));
        }

        let version = read_u32(data, 4)?;
        let mut offsets = [0u32; 8];
        for (i, off) in offsets.iter_mut().enumerate() {
            *off = read_u32(data, 8 + i * 4)?;
        }

        if offsets[0] != HEADER_SIZE as u32 {
            return Err(format!(
                "invalid PBP: PARAM.SFO must start at offset {}, header says {}",
                HEADER_SIZE, offsets[0]
            ));
        }
        for i in 0..8 {
            if offsets[i] as usize > data.len() {
                return Err(format!(
                    "invalid PBP: {} offset {} is past end of file ({} bytes)",
                    SLOT_NAMES[i],
                    offsets[i],
                    data.len()
                ));
            }
            if i > 0 && offsets[i] < offsets[i - 1] {
                return Err(format!(
                    "invalid PBP: {} offset {} precedes previous slot offset {}",
                    SLOT_NAMES[i],
                    offsets[i],
                    offsets[i - 1]
                ));
            }
        }

        // An absent slot stores the *previous present file's* offset. Therefore
        // a slot is present only when its offset increases relative to the
        // previous slot, and its length ends at the next strictly greater
        // offset (or at EOF for the final present file).
        let mut files: [Option<PbpFile>; 8] = std::array::from_fn(|_| None);
        for i in 0..8 {
            let is_present = i == 0 || offsets[i] > offsets[i - 1];
            if !is_present {
                continue;
            }
            let start = offsets[i] as usize;
            let end = (i + 1..8)
                .find(|&j| offsets[j] > offsets[i])
                .map(|j| offsets[j] as usize)
                .unwrap_or(data.len());
            if start > end {
                return Err(format!(
                    "invalid PBP: bad range for {} ({}..{})",
                    SLOT_NAMES[i], start, end
                ));
            }
            let size = end - start;
            if size > 0 {
                files[i] = Some(PbpFile {
                    slot: i,
                    name: SLOT_NAMES[i],
                    offset: offsets[i],
                    size,
                    data: data[start..end].to_vec(),
                });
            }
        }

        if files[0].is_none() {
            return Err("invalid PBP: PARAM.SFO slot is empty".to_string());
        }
        if present_count(&files) < 2 {
            return Err(
                "Every PBP archive needs at least a PARAM.SFO and one other file.".to_string(),
            );
        }

        Ok(Self {
            magic,
            version,
            offsets,
            files,
        })
    }

    pub fn from_file(path: &Path) -> Result<Self, String> {
        let data =
            fs::read(path).map_err(|e| format!("unable to read {}: {}", path.display(), e))?;
        Self::parse(&data)
    }

    pub fn present(&self) -> impl Iterator<Item = &PbpFile> {
        self.files.iter().filter_map(|f| f.as_ref())
    }

    pub fn get(&self, name: &str) -> Option<&PbpFile> {
        let upper = name.to_ascii_uppercase();
        let idx = SLOT_NAMES.iter().position(|n| *n == upper)?;
        self.files[idx].as_ref()
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        build(self.version, &self.files)
    }

    /// Write this archive to `path`.
    pub fn write_file(&self, path: &Path) -> Result<(), String> {
        let bytes = self.to_bytes()?;
        fs::write(path, bytes).map_err(|e| format!("unable to write {}: {}", path.display(), e))
    }
}

/// Build a PBP archive from slot contents. `files[i] == None` means empty slot.
pub fn build(version: u32, files: &[Option<PbpFile>; 8]) -> Result<Vec<u8>, String> {
    if files[0].is_none() {
        return Err("a PBP archive needs a PARAM.SFO file".to_string());
    }
    if files.iter().filter(|f| f.is_some()).count() < 2 {
        return Err("Every PBP archive needs at least a PARAM.SFO and one other file.".to_string());
    }

    let mut body = Vec::new();
    let mut offsets = [0u32; 8];
    let mut cursor = HEADER_SIZE as u64;
    let mut last_start = HEADER_SIZE as u32;

    for (i, slot) in files.iter().enumerate() {
        match slot {
            Some(f) => {
                let start = u32::try_from(cursor).map_err(|_| {
                    format!("{} is too large for a 32-bit PBP offset", SLOT_NAMES[i])
                })?;
                offsets[i] = start;
                last_start = start;
                body.extend_from_slice(&f.data);
                cursor += f.data.len() as u64;
            }
            None => {
                // Absent slots reuse the previous present file's offset.
                offsets[i] = last_start;
            }
        }
    }

    let total = HEADER_SIZE as u64 + body.len() as u64;
    if total > u32::MAX as u64 {
        return Err("PBP archive would exceed 4 GiB".to_string());
    }

    let mut out = Vec::with_capacity(body.len() + HEADER_SIZE);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    for off in offsets {
        out.extend_from_slice(&off.to_le_bytes());
    }
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn present_count(files: &[Option<PbpFile>; 8]) -> usize {
    files.iter().filter(|f| f.is_some()).count()
}

/// Map a file name (case-insensitive) to a PBP slot index.
pub fn slot_for_filename(name: &str) -> Option<usize> {
    let file_name = Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(name)
        .to_ascii_uppercase();
    SLOT_NAMES.iter().position(|n| *n == file_name)
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{:02X}", b));
    }
    s
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, String> {
    let bytes: [u8; 4] = data
        .get(offset..offset + 4)
        .ok_or_else(|| format!("unexpected end of PBP file at offset {}", offset))?
        .try_into()
        .map_err(|_| "internal error: bad u32 slice".to_string())?;
    Ok(u32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(i: usize, data: &[u8]) -> Option<PbpFile> {
        Some(PbpFile {
            slot: i,
            name: SLOT_NAMES[i],
            offset: 0,
            size: data.len(),
            data: data.to_vec(),
        })
    }

    #[test]
    fn roundtrip_two_slots() {
        let mut files: [Option<PbpFile>; 8] = std::array::from_fn(|_| None);
        files[0] = slot(0, b"sfo");
        files[1] = slot(1, b"png");
        let bytes = build(DEFAULT_VERSION, &files).unwrap();
        assert_eq!(bytes.len(), HEADER_SIZE + 6);
        assert_eq!(&bytes[0..4], &MAGIC);
        assert_eq!(&bytes[8..12], &40u32.to_le_bytes());
        assert_eq!(&bytes[12..16], &43u32.to_le_bytes());

        let parsed = PbpArchive::parse(&bytes).unwrap();
        assert_eq!(parsed.present().count(), 2);
        assert_eq!(parsed.get("ICON0.PNG").unwrap().data, b"png");
    }

    #[test]
    fn absent_slots_reuse_previous_present_offset() {
        let mut files: [Option<PbpFile>; 8] = std::array::from_fn(|_| None);
        files[0] = slot(0, b"sfo");
        files[1] = slot(1, b"icon");
        files[6] = slot(6, b"psp");
        let bytes = build(DEFAULT_VERSION, &files).unwrap();

        // PARAM.SFO@40, ICON0@43, empty slots 2..5 reuse 43,
        // DATA.PSP@47, trailing empty slot reuses 47.
        let off = |i: usize| u32::from_le_bytes(bytes[8 + i * 4..12 + i * 4].try_into().unwrap());
        assert_eq!(
            (off(0), off(1), off(2), off(5), off(6), off(7)),
            (40, 43, 43, 43, 47, 47)
        );

        let parsed = PbpArchive::parse(&bytes).unwrap();
        assert_eq!(parsed.present().count(), 3);
        assert_eq!(parsed.get("ICON0.PNG").unwrap().data, b"icon");
        assert_eq!(parsed.get("DATA.PSP").unwrap().data, b"psp");
        assert!(parsed.get("SND0.AT3").is_none());
    }

    #[test]
    fn accepts_real_and_legacy_magic() {
        let mut files: [Option<PbpFile>; 8] = std::array::from_fn(|_| None);
        files[0] = slot(0, b"sfo");
        files[1] = slot(1, b"png");
        let mut bytes = build(DEFAULT_VERSION, &files).unwrap();
        assert_eq!(&bytes[0..4], b"\0PBP");

        bytes[2] = b'S'; // old-style magic, just to be sure it still works
        let parsed = PbpArchive::parse(&bytes).unwrap();
        assert_eq!(parsed.magic, *b"\0PSP");
        assert_eq!(parsed.present().count(), 2);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = vec![0u8; HEADER_SIZE + 4];
        bytes[0] = b'X';
        let err = PbpArchive::parse(&bytes).unwrap_err();
        assert!(err.contains("magic"), "{}", err);
    }

    #[test]
    fn rejects_single_file() {
        let mut files: [Option<PbpFile>; 8] = std::array::from_fn(|_| None);
        files[0] = slot(0, b"sfo");
        assert!(build(DEFAULT_VERSION, &files).is_err());
    }
}
