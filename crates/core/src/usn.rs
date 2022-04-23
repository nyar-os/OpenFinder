//! USN journal access: change records, incremental cursor, and rescan detection.

use serde::Serialize;

use crate::{
    error::{Error, Result},
    volume::Volume,
};

/// `FSCTL_QUERY_USN_JOURNAL`.
const FSCTL_QUERY_USN_JOURNAL: u32 = 0x0009_00F4;
/// `FSCTL_READ_USN_JOURNAL`.
const FSCTL_READ_USN_JOURNAL: u32 = 0x0009_00BB;

/// Journal state reported by `FSCTL_QUERY_USN_JOURNAL`.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalState {
    /// Journal id required when reading.
    pub id: u64,
    /// Oldest USN still available.
    pub first_usn: u64,
    /// Next USN the volume will write.
    pub next_usn: u64,
}

/// One parsed `USN_RECORD_V2`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsnRecord {
    /// File record number (the low 48 bits of the file reference).
    pub frn: u64,
    /// Parent file record number.
    pub parent_frn: u64,
    /// Update sequence number of this change.
    pub usn: u64,
    /// Change timestamp in Unix milliseconds.
    pub timestamp_ms: u64,
    /// USN reason flags.
    pub reason: u32,
    /// File attributes.
    pub attributes: u32,
    /// File name at the time of the change.
    pub name: String,
}

/// Reason: file or directory created.
pub const REASON_FILE_CREATE: u32 = 0x0000_0100;
/// Reason: file or directory deleted.
pub const REASON_FILE_DELETE: u32 = 0x0000_0200;
/// Reason: new name after a rename.
pub const REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
/// Reason: standard information changed.
pub const REASON_BASIC_INFO_CHANGE: u32 = 0x0000_8000;
/// Reason: any data write, extend, or truncation.
pub const REASON_DATA: u32 = 0x0000_0007;

impl UsnRecord {
    /// Whether this record removes an entry.
    pub const fn is_delete(&self) -> bool {
        self.reason & REASON_FILE_DELETE != 0
    }

    /// Whether this record requires re-reading the MFT record.
    pub const fn requires_reload(&self) -> bool {
        self.reason & (REASON_FILE_CREATE | REASON_RENAME_NEW_NAME | REASON_BASIC_INFO_CHANGE | REASON_DATA) != 0
    }
}

/// Parse the output buffer of `FSCTL_READ_USN_JOURNAL`.
///
/// Returns the next start cursor and the decoded records.
pub fn parse_read_output(bytes: &[u8]) -> Result<(u64, Vec<UsnRecord>)> {
    if bytes.len() < 8 {
        return Err(Error::message("USN journal read output is shorter than its cursor"));
    }
    let next = u64::from_le_bytes(bytes[0..8].try_into().map_err(|_| Error::message("invalid USN cursor"))?);
    let mut records = Vec::new();
    let mut offset = 8usize;
    while offset + 60 <= bytes.len() {
        let length = read_u32(bytes, offset);
        if length == 0 || offset + length as usize > bytes.len() {
            break;
        }
        let major = read_u16(bytes, offset + 4);
        if major != 2 {
            offset += length as usize;
            continue;
        }
        records.push(parse_record(bytes, offset, length as usize)?);
        offset += length as usize;
    }
    Ok((next, records))
}

fn parse_record(bytes: &[u8], offset: usize, length: usize) -> Result<UsnRecord> {
    let file_reference = read_u64(bytes, offset + 8);
    let parent_reference = read_u64(bytes, offset + 16);
    let name_len = read_u16(bytes, offset + 56) as usize;
    let name_offset = read_u16(bytes, offset + 58) as usize;
    if name_len == 0 || offset + name_offset + name_len * 2 > length + offset {
        return Err(Error::message("USN record name is out of bounds"));
    }
    let mut units = Vec::with_capacity(name_len);
    for index in 0..name_len {
        let position = offset + name_offset + index * 2;
        units.push(u16::from_le_bytes([bytes[position], bytes[position + 1]]));
    }
    Ok(UsnRecord {
        frn: file_reference & 0x0000_FFFF_FFFF_FFFF,
        parent_frn: parent_reference & 0x0000_FFFF_FFFF_FFFF,
        usn: read_u64(bytes, offset + 24),
        timestamp_ms: filetime_to_unix_ms(read_u64(bytes, offset + 32)),
        reason: read_u32(bytes, offset + 40),
        attributes: read_u32(bytes, offset + 52),
        name: String::from_utf16_lossy(&units),
    })
}

/// Query the USN journal state.
pub fn query(volume: &mut Volume) -> Result<JournalState> {
    let output = volume.ioctl(FSCTL_QUERY_USN_JOURNAL, &[], 64)?;
    if output.len() < 24 {
        return Err(Error::message("USN journal state output is truncated"));
    }
    Ok(JournalState {
        id: u64::from_le_bytes(output[0..8].try_into().map_err(|_| Error::message("invalid journal id"))?),
        first_usn: u64::from_le_bytes(output[8..16].try_into().map_err(|_| Error::message("invalid first usn"))?),
        next_usn: u64::from_le_bytes(output[16..24].try_into().map_err(|_| Error::message("invalid next usn"))?),
    })
}

/// Read one batch of USN records starting at `start_usn`.
pub fn read(volume: &mut Volume, journal: &JournalState, start_usn: u64, wait_bytes: u32) -> Result<(u64, Vec<UsnRecord>)> {
    let mut input = [0u8; 32];
    input[0..8].copy_from_slice(&start_usn.to_le_bytes());
    input[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    input[12..16].copy_from_slice(&1u32.to_le_bytes());
    input[16..20].copy_from_slice(&0u32.to_le_bytes());
    input[20..24].copy_from_slice(&wait_bytes.to_le_bytes());
    input[24..32].copy_from_slice(&journal.id.to_le_bytes());
    let output = volume.ioctl(FSCTL_READ_USN_JOURNAL, &input, wait_bytes)?;
    parse_read_output(&output)
}

fn filetime_to_unix_ms(value: u64) -> u64 {
    if value == 0 {
        return 0;
    }
    value.saturating_div(10_000).saturating_sub(11_644_473_600_000)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(usn: u64, name: &str, reason: u32) -> Vec<u8> {
        let name_len = name.len();
        let length = 60 + name_len * 2;
        let mut bytes = vec![0u8; length];
        bytes[0..4].copy_from_slice(&(length as u32).to_le_bytes());
        bytes[4..6].copy_from_slice(&2u16.to_le_bytes());
        bytes[8..16].copy_from_slice(&(9u64 | (7u64 << 48)).to_le_bytes());
        bytes[16..24].copy_from_slice(&5u64.to_le_bytes());
        bytes[24..32].copy_from_slice(&usn.to_le_bytes());
        bytes[32..40].copy_from_slice(&421_243_200_000_000_000u64.to_le_bytes());
        bytes[40..44].copy_from_slice(&reason.to_le_bytes());
        bytes[52..56].copy_from_slice(&0u32.to_le_bytes());
        bytes[56..58].copy_from_slice(&(name_len as u16).to_le_bytes());
        bytes[58..60].copy_from_slice(&60u16.to_le_bytes());
        for (index, byte) in name.as_bytes().iter().enumerate() {
            bytes[60 + index * 2..60 + index * 2 + 2].copy_from_slice(&(*byte as u16).to_le_bytes());
        }
        bytes
    }

    #[test]
    fn read_output_decodes_v2_records() {
        let mut output = vec![0u8; 8];
        output[0..8].copy_from_slice(&101u64.to_le_bytes());
        output.extend(fixture(100, "alpha.txt", REASON_FILE_CREATE));
        output.extend(fixture(101, "beta.txt", REASON_FILE_DELETE));
        let (next, records) = parse_read_output(&output).unwrap();
        assert_eq!(next, 101);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].frn, 9);
        assert_eq!(records[0].name, "alpha.txt");
        assert!(records[0].requires_reload());
        assert!(records[1].is_delete());
        assert_eq!(records[1].parent_frn, 5);
    }

    #[test]
    fn read_output_stops_at_truncated_record() {
        let mut output = vec![0u8; 8];
        output.extend(fixture(1, "one.txt", REASON_FILE_CREATE));
        output.truncate(output.len() - 3);
        let (_, records) = parse_read_output(&output).unwrap();
        assert_eq!(records.len(), 0);
    }
}
