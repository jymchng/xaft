//! A minimal ZIP writer — all an EPUB needs.
//!
//! Every entry is *stored* (no compression). That is required for an EPUB's
//! first entry, `mimetype`, and valid for every other one; a book's text is
//! small and its images are already compressed, so deflate would buy little
//! and cost a dependency. Timestamps are fixed (1980-01-01), so the archive's
//! bytes depend only on its entries.

/// One archive member.
struct Entry {
    name: String,
    crc: u32,
    size: u32,
    offset: u32,
}

/// An in-memory ZIP archive being written.
#[derive(Default)]
pub(crate) struct ZipWriter {
    bytes: Vec<u8>,
    entries: Vec<Entry>,
}

/// CRC-32 (IEEE 802.3), as ZIP requires.
pub(crate) fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        table
    });
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc = table[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// DOS date for 1980-01-01.
const DOS_DATE: u16 = (1 << 5) | 1;

impl ZipWriter {
    /// Append one stored member.
    pub(crate) fn add(&mut self, name: &str, data: &[u8]) -> Result<(), String> {
        if self.entries.iter().any(|e| e.name == name) {
            return Err(format!("duplicate archive entry {name}"));
        }
        let size = u32::try_from(data.len()).map_err(|_| format!("{name} is too large"))?;
        let offset =
            u32::try_from(self.bytes.len()).map_err(|_| "archive is too large".to_string())?;
        let crc = crc32(data);
        let flags: u16 = if name.is_ascii() { 0 } else { 0x0800 };
        let b = &mut self.bytes;
        b.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        b.extend_from_slice(&20u16.to_le_bytes()); // version needed
        b.extend_from_slice(&flags.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes()); // stored
        b.extend_from_slice(&0u16.to_le_bytes()); // time
        b.extend_from_slice(&DOS_DATE.to_le_bytes());
        b.extend_from_slice(&crc.to_le_bytes());
        b.extend_from_slice(&size.to_le_bytes());
        b.extend_from_slice(&size.to_le_bytes());
        b.extend_from_slice(&(name.len() as u16).to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes()); // no extra field
        b.extend_from_slice(name.as_bytes());
        b.extend_from_slice(data);
        self.entries.push(Entry {
            name: name.to_string(),
            crc,
            size,
            offset,
        });
        Ok(())
    }

    /// Write the central directory and return the archive.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        let cd_start = self.bytes.len() as u32;
        for e in &self.entries {
            let flags: u16 = if e.name.is_ascii() { 0 } else { 0x0800 };
            let b = &mut self.bytes;
            b.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            b.extend_from_slice(&20u16.to_le_bytes()); // made by
            b.extend_from_slice(&20u16.to_le_bytes()); // needed
            b.extend_from_slice(&flags.to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
            b.extend_from_slice(&DOS_DATE.to_le_bytes());
            b.extend_from_slice(&e.crc.to_le_bytes());
            b.extend_from_slice(&e.size.to_le_bytes());
            b.extend_from_slice(&e.size.to_le_bytes());
            b.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes()); // extra
            b.extend_from_slice(&0u16.to_le_bytes()); // comment
            b.extend_from_slice(&0u16.to_le_bytes()); // disk
            b.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            b.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            b.extend_from_slice(&e.offset.to_le_bytes());
            b.extend_from_slice(e.name.as_bytes());
        }
        let cd_size = self.bytes.len() as u32 - cd_start;
        let count = self.entries.len() as u16;
        let b = &mut self.bytes;
        b.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        b.extend_from_slice(&cd_size.to_le_bytes());
        b.extend_from_slice(&cd_start.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        self.bytes
    }
}

/// The members of a stored ZIP archive, in order: `(name, data)`.
///
/// Reads what [`ZipWriter`] writes (stored entries); used to validate a
/// built EPUB. Any compressed entry is reported as an error.
pub(crate) fn read_stored(archive: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let u16_at = |i: usize| -> Result<u16, String> {
        archive
            .get(i..i + 2)
            .map(|s| u16::from_le_bytes([s[0], s[1]]))
            .ok_or_else(|| "truncated archive".to_string())
    };
    let u32_at = |i: usize| -> Result<u32, String> {
        archive
            .get(i..i + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
            .ok_or_else(|| "truncated archive".to_string())
    };
    let mut entries = Vec::new();
    let mut i = 0usize;
    while i + 4 <= archive.len() && u32_at(i)? == 0x0403_4b50 {
        let method = u16_at(i + 8)?;
        let size = u32_at(i + 18)? as usize;
        let name_len = u16_at(i + 26)? as usize;
        let extra_len = u16_at(i + 28)? as usize;
        let name_start = i + 30;
        let data_start = name_start + name_len + extra_len;
        let name = archive
            .get(name_start..name_start + name_len)
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .ok_or("truncated archive")?;
        if method != 0 {
            return Err(format!(
                "{name} is compressed; only stored entries are read"
            ));
        }
        let data = archive
            .get(data_start..data_start + size)
            .ok_or("truncated archive")?
            .to_vec();
        entries.push((name, data));
        i = data_start + size;
    }
    if entries.is_empty() {
        return Err("not a ZIP archive".into());
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_reference_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn written_archives_read_back_in_order() {
        let mut zip = ZipWriter::default();
        zip.add("mimetype", b"application/epub+zip").unwrap();
        zip.add("OEBPS/a.xhtml", b"<html/>").unwrap();
        assert!(zip.add("mimetype", b"x").is_err());
        let bytes = zip.finish();
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        assert_eq!(
            &bytes[30..38],
            b"mimetype",
            "mimetype is first, without an extra field"
        );
        let entries = read_stored(&bytes).unwrap();
        assert_eq!(
            entries[0],
            ("mimetype".to_string(), b"application/epub+zip".to_vec())
        );
        assert_eq!(entries[1].0, "OEBPS/a.xhtml");
        assert!(read_stored(b"not a zip").is_err());
    }

    #[test]
    fn the_archive_is_readable_by_unzip_when_available() {
        let Ok(probe) = std::process::Command::new("unzip").arg("-v").output() else {
            return;
        };
        if !probe.status.success() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut zip = ZipWriter::default();
        zip.add("mimetype", b"application/epub+zip").unwrap();
        zip.add("dir/file.txt", b"hello").unwrap();
        let path = dir.path().join("t.zip");
        std::fs::write(&path, zip.finish()).unwrap();
        let out = std::process::Command::new("unzip")
            .args(["-t", "-q"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "unzip -t: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}
