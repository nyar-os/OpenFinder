//! NTFS on-disk structures: boot sector, record fixups, attributes, data runs.
//!
//! Everything is parsed from raw bytes so a volume can be indexed without any
//! third-party NTFS library. All offsets follow the published NTFS layout.

use crate::{
    error::{Error, Result},
    index::IndexEntry,
};

/// NTFS sector size used by the update sequence array. NTFS 3.1 mandates 512.
pub(crate) const NTFS_SECTOR_SIZE: usize = 512;

const RECORD_SIGNATURE: &[u8; 4] = b"FILE";
const ATTR_END: u32 = 0xFFFF_FFFF;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_DATA: u32 = 0x80;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
/// File records carry a 48-bit record number plus a 16-bit sequence number.
const FRN_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// Parsed NTFS boot sector fields needed for raw indexing.
#[derive(Debug, Clone, Copy)]
pub struct BootSector {
    /// Bytes per sector.
    pub bytes_per_sector: u16,
    /// Sectors per cluster.
    pub sectors_per_cluster: u8,
    /// Logical cluster number of the `$MFT` start.
    pub mft_lcn: u64,
    /// Clusters per file record segment; negative means `1 << -value`.
    pub clusters_per_record: i8,
    /// Volume serial number.
    pub volume_serial: u64,
}

impl BootSector {
    /// Parse the first sector of an NTFS volume.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 512 {
            return Err(Error::message("boot sector is shorter than 512 bytes"));
        }
        let bytes_per_sector = read_u16(bytes, 0x0B);
        let sectors_per_cluster = bytes[0x0D];
        if bytes_per_sector == 0 || !bytes_per_sector.is_power_of_two() || sectors_per_cluster == 0 {
            return Err(Error::message("boot sector reports an invalid cluster geometry"));
        }
        Ok(Self {
            bytes_per_sector,
            sectors_per_cluster,
            mft_lcn: read_u64(bytes, 0x30),
            clusters_per_record: bytes[0x40] as i8,
            volume_serial: read_u64(bytes, 0x48),
        })
    }

    /// Cluster size in bytes.
    pub fn cluster_size(&self) -> u64 {
        u64::from(self.bytes_per_sector) * u64::from(self.sectors_per_cluster)
    }

    /// File record size in bytes.
    pub fn record_size(&self) -> Result<usize> {
        let raw = self.clusters_per_record;
        let size = if raw < 0 { 1usize << (-raw as u32) } else { raw as usize * self.cluster_size() as usize };
        if !(512..=65_536).contains(&size) {
            return Err(Error::message(format!("unsupported file record size {size}")));
        }
        Ok(size)
    }
}

/// One contiguous NTFS data run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataRun {
    /// Starting virtual cluster number.
    pub vcn: u64,
    /// Starting logical cluster number.
    pub lcn: u64,
    /// Number of clusters in the run.
    pub clusters: u64,
}

/// Parse an attribute data-run list.
pub fn parse_data_runs(bytes: &[u8], base_vcn: u64) -> Result<Vec<DataRun>> {
    let mut runs = Vec::new();
    let mut offset = 0usize;
    let mut vcn = base_vcn;
    let mut lcn: i64 = 0;
    while offset < bytes.len() {
        let header = bytes[offset];
        if header == 0 {
            break;
        }
        let length_bytes = (header & 0x0F) as usize;
        let offset_bytes = (header >> 4) as usize;
        if length_bytes == 0 || offset_bytes == 0 || length_bytes > 8 || offset_bytes > 8 {
            return Err(Error::message("malformed NTFS data run header"));
        }
        offset += 1;
        if offset + length_bytes + offset_bytes > bytes.len() {
            return Err(Error::message("truncated NTFS data run"));
        }
        let length = read_unsigned(&bytes[offset..offset + length_bytes]);
        offset += length_bytes;
        let delta = read_signed(&bytes[offset..offset + offset_bytes]);
        offset += offset_bytes;
        if length == 0 {
            return Err(Error::message("NTFS data run has zero length"));
        }
        lcn = lcn.wrapping_add(delta);
        if lcn < 0 {
            return Err(Error::message("NTFS data run resolves to a negative cluster"));
        }
        runs.push(DataRun { vcn, lcn: lcn as u64, clusters: length });
        vcn += length;
    }
    Ok(runs)
}

/// Unnamed `$DATA` stream of a record.
#[derive(Debug, Clone)]
pub struct DataStream {
    /// Logical size in bytes.
    pub size: u64,
    /// Data runs when the stream is non-resident.
    pub runs: Vec<DataRun>,
}

/// One parsed MFT file record.
#[derive(Debug, Clone)]
pub struct ParsedRecord {
    /// `true` when the record is allocated.
    pub in_use: bool,
    /// Index entry when the record carries a `$FILE_NAME`.
    pub entry: Option<IndexEntry>,
    /// Unnamed data stream, when present.
    pub data: Option<DataStream>,
}

/// Parse one MFT record segment.
///
/// `frn` is the record number used when the record itself is unreadable; the
/// returned entry always uses the record header's own sequence number.
pub fn parse_record(bytes: &[u8], sector_size: usize, frn: u64) -> Result<ParsedRecord> {
    if bytes.len() < 48 || &bytes[0..4] != RECORD_SIGNATURE {
        return Ok(ParsedRecord { in_use: false, entry: None, data: None });
    }
    let usa_offset = read_u16(bytes, 0x04) as usize;
    let usa_count = read_u16(bytes, 0x06) as usize;
    if usa_count == 0 || usa_offset + usa_count * 2 > bytes.len() {
        return Err(Error::message("MFT record update sequence array is out of bounds"));
    }
    let mut repaired = bytes.to_vec();
    apply_fixups(&mut repaired, usa_offset, usa_count, sector_size)?;

    let flags = read_u16(&repaired, 0x16);
    let in_use = flags & 0x0001 != 0;
    let first_attr = read_u16(&repaired, 0x14) as usize;
    let used_size = (read_u32(&repaired, 0x18) as usize).min(repaired.len());
    let mut offset = first_attr;
    let mut file_name = None;
    let mut standard = None;
    let mut data = None;

    while offset + 8 <= used_size {
        let attr_type = read_u32(&repaired, offset);
        if attr_type == ATTR_END {
            break;
        }
        let attr_len = read_u32(&repaired, offset + 4) as usize;
        if attr_len < 16 || offset + attr_len > repaired.len() {
            return Err(Error::message("MFT record contains a malformed attribute"));
        }
        let non_resident = repaired[offset + 8] != 0;
        let name_len = repaired[offset + 9] as usize;
        if attr_type == ATTR_FILE_NAME && !non_resident && name_len == 0 {
            file_name = parse_file_name(&repaired[offset..offset + attr_len]);
        }
        else if attr_type == ATTR_STANDARD_INFORMATION && !non_resident && name_len == 0 {
            standard = parse_standard_information(&repaired[offset..offset + attr_len]);
        }
        else if attr_type == ATTR_DATA && name_len == 0 {
            data = parse_data_stream(&repaired[offset..offset + attr_len]);
        }
        offset += attr_len;
    }

    let Some(name) = file_name
    else {
        return Ok(ParsedRecord { in_use, entry: None, data });
    };
    let (created_ms, modified_ms, accessed_ms) = standard.unwrap_or((name.created_ms, name.modified_ms, name.accessed_ms));
    let is_dir = name.flags & FILE_ATTRIBUTE_DIRECTORY != 0;
    let size = if is_dir { 0 } else { data.as_ref().map_or(name.size, |stream| stream.size) };
    let entry = IndexEntry { frn, parent_frn: name.parent_frn, name: name.name, is_dir, size, created_ms, modified_ms, accessed_ms };
    Ok(ParsedRecord { in_use, entry: Some(entry), data })
}

fn apply_fixups(bytes: &mut [u8], usa_offset: usize, usa_count: usize, sector_size: usize) -> Result<()> {
    if usa_count < 2 {
        return Ok(());
    }
    let update_sequence = read_u16(bytes, usa_offset);
    for index in 1..usa_count {
        let end = index * sector_size;
        let start = end - 2;
        if end > bytes.len() {
            break;
        }
        if read_u16(bytes, start) != update_sequence {
            return Err(Error::message("MFT record fixup does not match the update sequence number"));
        }
        let replacement = read_u16(bytes, usa_offset + index * 2);
        bytes[start..end].copy_from_slice(&replacement.to_le_bytes());
    }
    Ok(())
}

struct FileNameAttribute {
    parent_frn: u64,
    created_ms: u64,
    modified_ms: u64,
    accessed_ms: u64,
    size: u64,
    flags: u32,
    name: String,
}

fn parse_file_name(attribute: &[u8]) -> Option<FileNameAttribute> {
    let value_offset = read_u16(attribute, 0x14) as usize;
    let value_len = read_u32(attribute, 0x10) as usize;
    if value_offset == 0 || value_offset + value_len > attribute.len() || value_len < 66 {
        return None;
    }
    let value = &attribute[value_offset..value_offset + value_len];
    let name_len = value[64] as usize;
    if name_len == 0 || 66 + name_len * 2 > value.len() {
        return None;
    }
    Some(FileNameAttribute {
        parent_frn: read_u64(value, 0) & FRN_MASK,
        created_ms: filetime_to_unix_ms(read_u64(value, 8)),
        modified_ms: filetime_to_unix_ms(read_u64(value, 16)),
        accessed_ms: filetime_to_unix_ms(read_u64(value, 32)),
        size: read_u64(value, 48),
        flags: read_u32(value, 56),
        name: decode_utf16(&value[66..66 + name_len * 2]),
    })
}

fn parse_standard_information(attribute: &[u8]) -> Option<(u64, u64, u64)> {
    let value_offset = read_u16(attribute, 0x14) as usize;
    let value_len = read_u32(attribute, 0x10) as usize;
    if value_offset == 0 || value_offset + value_len > attribute.len() || value_len < 32 {
        return None;
    }
    let value = &attribute[value_offset..value_offset + value_len];
    Some((filetime_to_unix_ms(read_u64(value, 0)), filetime_to_unix_ms(read_u64(value, 8)), filetime_to_unix_ms(read_u64(value, 24))))
}

fn parse_data_stream(attribute: &[u8]) -> Option<DataStream> {
    if attribute[8] != 0 {
        let runs_offset = read_u16(attribute, 0x20) as usize;
        let real_size = read_u64(attribute, 0x30);
        if runs_offset == 0 || runs_offset > attribute.len() {
            return None;
        }
        let base_vcn = read_u64(attribute, 0x10);
        let runs = parse_data_runs(&attribute[runs_offset..], base_vcn).ok()?;
        return Some(DataStream { size: real_size, runs });
    }
    let value_len = read_u32(attribute, 0x10);
    Some(DataStream { size: u64::from(value_len), runs: Vec::new() })
}

fn filetime_to_unix_ms(value: u64) -> u64 {
    if value == 0 {
        return 0;
    }
    value.saturating_div(10_000).saturating_sub(11_644_473_600_000)
}

fn decode_utf16(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.as_chunks::<2>().0 {
        units.push(u16::from_le_bytes(*chunk));
    }
    String::from_utf16_lossy(&units)
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(value)
}

fn read_unsigned(bytes: &[u8]) -> u64 {
    let mut value = 0u64;
    for (index, byte) in bytes.iter().enumerate() {
        value |= u64::from(*byte) << (index * 8);
    }
    value
}

fn read_signed(bytes: &[u8]) -> i64 {
    let mut value: i64 = 0;
    for (index, byte) in bytes.iter().enumerate() {
        value |= i64::from(*byte) << (index * 8);
    }
    let bits = (bytes.len() * 8) as u32;
    if bits < 64 && value & (1i64 << (bits - 1)) != 0 {
        value -= 1i64 << bits;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boot_sector() -> Vec<u8> {
        let mut bytes = vec![0u8; 512];
        bytes[0..3].copy_from_slice(&[0xEB, 0x52, 0x90]);
        bytes[3..11].copy_from_slice(b"NTFS    ");
        bytes[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        bytes[0x0D] = 8;
        bytes[0x28..0x30].copy_from_slice(&100_000u64.to_le_bytes());
        bytes[0x30..0x38].copy_from_slice(&4u64.to_le_bytes());
        bytes[0x40] = 0xF6; // -10 -> 1024 bytes
        bytes[0x48..0x50].copy_from_slice(&42u64.to_le_bytes());
        bytes
    }

    #[test]
    fn boot_sector_reports_geometry() {
        let parsed = BootSector::parse(&boot_sector()).unwrap();
        assert_eq!(parsed.bytes_per_sector, 512);
        assert_eq!(parsed.cluster_size(), 4096);
        assert_eq!(parsed.record_size().unwrap(), 1024);
        assert_eq!(parsed.volume_serial, 42);
        assert_eq!(parsed.mft_lcn, 4);
    }

    #[test]
    fn data_runs_decode_relative_offsets() {
        let bytes = [0x11, 0x02, 0x21, 0x11, 0x05, 0x07, 0x00];
        let runs = parse_data_runs(&bytes, 0).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0], DataRun { vcn: 0, lcn: 0x21, clusters: 0x02 });
        assert_eq!(runs[1], DataRun { vcn: 2, lcn: 0x28, clusters: 0x05 });
    }

    #[test]
    fn record_parses_name_size_and_times() {
        let record = fixture_record();
        let parsed = parse_record(&record, NTFS_SECTOR_SIZE, 7).unwrap();
        assert!(parsed.in_use);
        let entry = parsed.entry.unwrap();
        assert_eq!(entry.frn, 7);
        assert_eq!(entry.name, "hello.txt");
        assert_eq!(entry.size, 1234);
        assert!(!entry.is_dir);
        assert_eq!(entry.parent_frn, 5);
    }

    #[test]
    fn records_without_file_name_are_skipped() {
        let mut record = fixture_record();
        record[0x14..0x16].copy_from_slice(&u16::MAX.to_le_bytes());
        let parsed = parse_record(&record, NTFS_SECTOR_SIZE, 9).unwrap();
        assert!(parsed.entry.is_none());
    }

    fn fixture_record() -> Vec<u8> {
        let mut record = vec![0u8; 1024];
        record[0..4].copy_from_slice(RECORD_SIGNATURE);
        record[0x04..0x06].copy_from_slice(&0x30u16.to_le_bytes());
        record[0x06..0x08].copy_from_slice(&3u16.to_le_bytes());
        record[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
        record[0x16..0x18].copy_from_slice(&1u16.to_le_bytes());
        record[0x18..0x1C].copy_from_slice(&1024u32.to_le_bytes());
        record[0x30..0x32].copy_from_slice(&0x0001u16.to_le_bytes());
        record[0x32..0x34].copy_from_slice(&0x0002u16.to_le_bytes());
        for sector in 1..3 {
            let end = sector * NTFS_SECTOR_SIZE;
            record[end - 2..end].copy_from_slice(&0x0001u16.to_le_bytes());
        }

        let mut offset = 0x38usize;
        // $STANDARD_INFORMATION (resident, 32-byte value slot).
        write_attr(&mut record, offset, ATTR_STANDARD_INFORMATION, 56, true);
        let value_offset = offset + 24;
        record[offset + 0x14..offset + 0x16].copy_from_slice(&24u16.to_le_bytes());
        record[offset + 0x10..offset + 0x14].copy_from_slice(&32u32.to_le_bytes());
        record[value_offset..value_offset + 8].copy_from_slice(&421_243_200_000_000_000u64.to_le_bytes());
        offset += 56;

        // $FILE_NAME (resident, 84-byte value for a 9-character name).
        write_attr(&mut record, offset, ATTR_FILE_NAME, 112, true);
        let name_value = offset + 24;
        record[offset + 0x14..offset + 0x16].copy_from_slice(&24u16.to_le_bytes());
        record[offset + 0x10..offset + 0x14].copy_from_slice(&84u32.to_le_bytes());
        // Parent reference: record 5 with sequence 7 packed into the high bits.
        record[name_value..name_value + 8].copy_from_slice(&(5u64 | (7u64 << 48)).to_le_bytes());
        record[name_value + 8..name_value + 16].copy_from_slice(&421_243_200_000_000_000u64.to_le_bytes());
        record[name_value + 16..name_value + 24].copy_from_slice(&421_243_200_000_000_000u64.to_le_bytes());
        record[name_value + 32..name_value + 40].copy_from_slice(&421_243_200_000_000_000u64.to_le_bytes());
        record[name_value + 48..name_value + 56].copy_from_slice(&1234u64.to_le_bytes());
        record[name_value + 56..name_value + 60].copy_from_slice(&0u32.to_le_bytes());
        record[name_value + 64] = 9;
        record[name_value + 65] = 1;
        for (index, byte) in "hello.txt".as_bytes().iter().enumerate() {
            let position = name_value + 66 + index * 2;
            record[position..position + 2].copy_from_slice(&(*byte as u16).to_le_bytes());
        }
        offset += 112;

        // $DATA (non-resident, 1234 bytes in one cluster).
        write_attr(&mut record, offset, ATTR_DATA, 68, false);
        record[offset + 0x20..offset + 0x22].copy_from_slice(&0x40u16.to_le_bytes());
        record[offset + 0x30..offset + 0x38].copy_from_slice(&1234u64.to_le_bytes());
        record[offset + 0x40..offset + 0x44].copy_from_slice(&[0x11, 0x01, 0x02, 0x00]);
        offset += 68;
        record[offset..offset + 4].copy_from_slice(&ATTR_END.to_le_bytes());

        record[0x30..0x32].copy_from_slice(&0x0001u16.to_le_bytes());
        record[0x32..0x34].copy_from_slice(&0x0002u16.to_le_bytes());
        record
    }

    fn write_attr(record: &mut [u8], offset: usize, attr_type: u32, length: usize, resident: bool) {
        record[offset..offset + 4].copy_from_slice(&attr_type.to_le_bytes());
        record[offset + 4..offset + 8].copy_from_slice(&(length as u32).to_le_bytes());
        record[offset + 8] = u8::from(!resident);
        record[offset + 9] = 0;
        record[offset + 10..offset + 12].copy_from_slice(&0u16.to_le_bytes());
    }
}
