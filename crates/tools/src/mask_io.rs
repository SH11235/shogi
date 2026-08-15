//! PSV と LSB-first bitmap mask を組み合わせるツール向けの共通 I/O 検証。

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::packed_sfen::PackedSfenValue;

/// `<out>.partial` 形式の staging パスを返す。
pub fn partial_path(out: &Path) -> PathBuf {
    let mut path = out.as_os_str().to_os_string();
    path.push(".partial");
    PathBuf::from(path)
}

/// PSV のサイズを検証し、レコード数を返す。
pub fn checked_input_records(input: &Path) -> Result<u64> {
    let bytes = fs::metadata(input)
        .with_context(|| format!("Failed to stat input {}", input.display()))?
        .len();
    anyhow::ensure!(
        bytes % PackedSfenValue::SIZE as u64 == 0,
        "Input size is not a multiple of {}: {} ({bytes} bytes)",
        PackedSfenValue::SIZE,
        input.display()
    );
    Ok(bytes / PackedSfenValue::SIZE as u64)
}

/// mask のサイズと最終 byte の未使用 bit を検証する。
pub fn validate_mask(mask: &Path, records: u64) -> Result<()> {
    let expected_bytes = records.div_ceil(8);
    let actual_bytes = fs::metadata(mask)
        .with_context(|| format!("Failed to stat mask {}", mask.display()))?
        .len();
    anyhow::ensure!(
        actual_bytes == expected_bytes,
        "Mask size mismatch: expected {expected_bytes} bytes for {records} records, got {actual_bytes}: {}",
        mask.display()
    );

    let used_bits = (records % 8) as u32;
    if used_bits != 0 {
        let mut file = File::open(mask)?;
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0u8; 1];
        file.read_exact(&mut last)?;
        let unused_mask = !((1u8 << used_bits) - 1);
        anyhow::ensure!(
            last[0] & unused_mask == 0,
            "Mask has non-zero unused bits in final byte: {}",
            mask.display()
        );
    }
    Ok(())
}

/// chunk のレコード数が正かつ mask の byte 境界に揃うことを検証する。
pub fn validate_chunk_records(chunk_records: usize) -> Result<()> {
    anyhow::ensure!(chunk_records > 0, "chunk record count must be positive");
    anyhow::ensure!(
        chunk_records.is_multiple_of(8),
        "chunk record count must be a multiple of 8: {chunk_records}"
    );
    Ok(())
}
