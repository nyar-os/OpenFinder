//! Sequential and random access over the `$MFT` data stream.

use crate::{
    error::{Error, Result},
    index::IndexEntry,
    ntfs::{BootSector, DataRun, NTFS_SECTOR_SIZE, parse_record},
    volume::VolumeReader,
};

/// Batched MFT reader.
pub struct MftReader<R> {
    reader: R,
    boot: BootSector,
    record_size: usize,
    runs: Vec<DataRun>,
    record_count: u64,
}

impl<R: VolumeReader> MftReader<R> {
    /// Read the boot sector and record 0, then derive the MFT data runs.
    pub fn bootstrap(mut reader: R) -> Result<Self> {
        let mut sector = vec![0u8; 512];
        reader.read_at(0, &mut sector)?;
        let boot = BootSector::parse(&sector)?;
        let record_size = boot.record_size()?;
        let cluster_size = boot.cluster_size();
        let mut record_zero = vec![0u8; record_size];
        reader.read_at(boot.mft_lcn * cluster_size, &mut record_zero)?;
        let parsed = parse_record(&record_zero, NTFS_SECTOR_SIZE, 0)?;
        let Some(data) = parsed.data
        else {
            return Err(Error::message("$MFT record 0 has no unnamed data stream"));
        };
        if data.runs.is_empty() {
            return Err(Error::message("$MFT data stream is resident; a volume index cannot be built"));
        }
        let record_count = data.size / record_size as u64;
        if record_count == 0 {
            return Err(Error::message("$MFT data stream reports zero records"));
        }
        Ok(Self { reader, boot, record_size, runs: data.runs, record_count })
    }

    /// Number of record slots in the MFT data stream.
    pub const fn record_count(&self) -> u64 {
        self.record_count
    }

    /// Borrow the underlying volume for journal control calls.
    pub fn volume_mut(&mut self) -> &mut R {
        &mut self.reader
    }

    /// Read one record by record number.
    pub fn read_record(&mut self, frn: u64) -> Result<Option<IndexEntry>> {
        if frn >= self.record_count {
            return Ok(None);
        }
        let mut bytes = vec![0u8; self.record_size];
        let offset = frn * self.record_size as u64;
        self.read_stream(offset, &mut bytes)?;
        let parsed = parse_record(&bytes, NTFS_SECTOR_SIZE, frn)?;
        Ok(if parsed.in_use { parsed.entry } else { None })
    }

    /// Scan records in batches and yield allocated entries with a name.
    ///
    /// Returns the number of record slots examined. `record_limit` caps the scan
    /// so a caller can page over a very large MFT.
    pub fn scan<F>(&mut self, record_limit: Option<u64>, mut on_entry: F) -> Result<u64>
    where
        F: FnMut(IndexEntry),
    {
        const BATCH: u64 = 128;
        let limit = record_limit.unwrap_or(self.record_count).min(self.record_count);
        let mut scanned = 0u64;
        while scanned < limit {
            let count = BATCH.min(limit - scanned);
            let mut batch = vec![0u8; count as usize * self.record_size];
            self.read_stream(scanned * self.record_size as u64, &mut batch)?;
            for index in 0..count {
                let start = index as usize * self.record_size;
                let record = &batch[start..start + self.record_size];
                let parsed = parse_record(record, NTFS_SECTOR_SIZE, scanned + index)?;
                if parsed.in_use
                    && let Some(entry) = parsed.entry
                {
                    on_entry(entry);
                }
            }
            scanned += count;
        }
        Ok(scanned)
    }

    fn read_stream(&mut self, offset: u64, out: &mut [u8]) -> Result<()> {
        let cluster = self.boot.cluster_size();
        let mut position = offset;
        let mut written = 0usize;
        while written < out.len() {
            let vcn = position / cluster;
            let intra = position % cluster;
            let Some(run) = self.runs.iter().find(|run| vcn >= run.vcn && vcn < run.vcn + run.clusters)
            else {
                return Err(Error::message(format!("MFT data run does not cover virtual cluster {vcn}")));
            };
            let physical = run.lcn * cluster + intra;
            let available = ((run.vcn + run.clusters) * cluster - position) as usize;
            let take = available.min(out.len() - written);
            self.reader.read_at(physical, &mut out[written..written + take])?;
            written += take;
            position += take as u64;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLUSTER: usize = 4096;
    const MFT_LCN: usize = 4;

    #[test]
    fn bootstrap_reads_record_count_and_records() {
        let volume = fixture_volume();
        let mut reader = MftReader::bootstrap(volume).unwrap();
        assert_eq!(reader.record_count(), 12);
        let entry = reader.read_record(1).unwrap().unwrap();
        assert_eq!(entry.name, "report.txt");
        assert_eq!(entry.frn, 1);
        assert!(reader.read_record(50).unwrap().is_none());
    }

    #[test]
    fn scan_skips_unallocated_and_nameless_records() {
        let mut reader = MftReader::bootstrap(fixture_volume()).unwrap();
        let mut names = Vec::new();
        let scanned = reader.scan(Some(3), |entry| names.push(entry.name.clone())).unwrap();
        assert_eq!(scanned, 3);
        assert_eq!(names, vec!["report.txt".to_string()]);
    }

    fn fixture_volume() -> Vec<u8> {
        let data_start = MFT_LCN * CLUSTER;
        let data_len = 3 * CLUSTER;
        let mut bytes = vec![0u8; data_start + data_len];
        write_boot(&mut bytes);
        let mut record_zero = record_header();
        // $DATA non-resident: 3 clusters starting at cluster 4.
        let attr_len = 0x40 + 4;
        let offset = 0x38;
        record_zero[offset..offset + 4].copy_from_slice(&0x80u32.to_le_bytes());
        record_zero[offset + 4..offset + 8].copy_from_slice(&(attr_len as u32).to_le_bytes());
        record_zero[offset + 8] = 1;
        record_zero[offset + 0x10..offset + 0x18].copy_from_slice(&0u64.to_le_bytes());
        record_zero[offset + 0x18..offset + 0x20].copy_from_slice(&2u64.to_le_bytes());
        record_zero[offset + 0x20..offset + 0x22].copy_from_slice(&0x40u16.to_le_bytes());
        record_zero[offset + 0x28..offset + 0x30].copy_from_slice(&(data_len as u64).to_le_bytes());
        record_zero[offset + 0x30..offset + 0x38].copy_from_slice(&(data_len as u64).to_le_bytes());
        record_zero[offset + 0x38..offset + 0x40].copy_from_slice(&(data_len as u64).to_le_bytes());
        record_zero[offset + 0x40..offset + 0x44].copy_from_slice(&[0x11, 0x03, 0x04, 0x00]);
        record_zero[0x18..0x1C].copy_from_slice(&((offset + attr_len + 4) as u32).to_le_bytes());
        record_zero[offset + attr_len..offset + attr_len + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        bytes[data_start..data_start + 1024].copy_from_slice(&record_zero);

        let mut named = record_header();
        write_resident(&mut named, 0x38, 0x30, 86, |value| {
            value[0..8].copy_from_slice(&5u64.to_le_bytes());
            value[48..56].copy_from_slice(&77u64.to_le_bytes());
            value[64] = 10;
            value[65] = 1;
            for (index, byte) in b"report.txt".iter().enumerate() {
                value[66 + index * 2..66 + index * 2 + 2].copy_from_slice(&(*byte as u16).to_le_bytes());
            }
        });
        bytes[data_start + 1024..data_start + 2048].copy_from_slice(&named);
        bytes
    }

    fn record_header() -> Vec<u8> {
        let mut record = vec![0u8; 1024];
        record[0..4].copy_from_slice(b"FILE");
        record[0x04..0x06].copy_from_slice(&0x30u16.to_le_bytes());
        record[0x06..0x08].copy_from_slice(&3u16.to_le_bytes());
        record[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
        record[0x16..0x18].copy_from_slice(&1u16.to_le_bytes());
        record[0x18..0x1C].copy_from_slice(&1024u32.to_le_bytes());
        record[0x30..0x32].copy_from_slice(&1u16.to_le_bytes());
        record[0x32..0x34].copy_from_slice(&2u16.to_le_bytes());
        record[0x34..0x36].copy_from_slice(&2u16.to_le_bytes());
        for sector in 1..3 {
            let end = sector * 512;
            record[end - 2..end].copy_from_slice(&1u16.to_le_bytes());
        }
        record
    }

    fn write_resident(record: &mut [u8], offset: usize, attr_type: u32, value_len: usize, fill: impl FnOnce(&mut [u8])) {
        let total = 24 + value_len;
        record[offset..offset + 4].copy_from_slice(&attr_type.to_le_bytes());
        record[offset + 4..offset + 8].copy_from_slice(&(total as u32).to_le_bytes());
        record[offset + 0x10..offset + 0x14].copy_from_slice(&(value_len as u32).to_le_bytes());
        record[offset + 0x14..offset + 0x16].copy_from_slice(&24u16.to_le_bytes());
        fill(&mut record[offset + 24..offset + 24 + value_len]);
        record[offset + total..offset + total + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        record[0x18..0x1C].copy_from_slice(&((offset + total + 4) as u32).to_le_bytes());
    }

    fn write_boot(bytes: &mut [u8]) {
        bytes[0..3].copy_from_slice(&[0xEB, 0x52, 0x90]);
        bytes[3..11].copy_from_slice(b"NTFS    ");
        bytes[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        bytes[0x0D] = 8;
        bytes[0x30..0x38].copy_from_slice(&(MFT_LCN as u64).to_le_bytes());
        bytes[0x40] = 0xF6;
    }
}
