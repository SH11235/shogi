//! システムメモリ情報のクロスプラットフォーム取得。

use sysinfo::{MemoryRefreshKind, RefreshKind, System};

/// OS が報告する利用可能メモリをバイト単位で返す。
///
/// Linux、Windows、macOS を含む `sysinfo` 対応環境で利用できる。
/// OS から情報を取得できない場合や、取得値が 0 の場合は `None` を返す。
pub fn available_memory_bytes() -> Option<u64> {
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let available = system.available_memory();
    (available > 0).then_some(available)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_memory_is_nonzero_when_detected() {
        if let Some(available) = available_memory_bytes() {
            assert!(available > 0);
        }
    }
}
