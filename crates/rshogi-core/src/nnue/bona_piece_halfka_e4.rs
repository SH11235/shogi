//! HalfKa-E4 index 合成 (base HalfKaHmMerged index の bucket 拡張)
//!
//! E4 は base 特徴 index を各駒マスの「被攻撃数×被防御数」バケットで拡張する
//! (`e4_index = base_index * NB + bucket`)。base index 計算は
//! [`super::bona_piece_halfka_hm_merged`] と共有し、本モジュールは bucket 量子化と
//! index 合成のみを担う。
//!
//! # クロスリポジトリ契約
//!
//! bucket 量子化・index 合成・bucketed 判定は学習側 (rshogi-nnue/shogi-features) と
//! bit 一致させる必要がある。契約は ADR `2026-07-09-halfka-e4-feature-set.md`。
//! - `NB` と量子化の clip 段数
//! - `packed_is_bucketed`: どの packed BonaPiece 域がバケット化されるか
//! - `e4_index = base_index * NB + bucket`

use super::bona_piece_halfka_hm_merged::PIECE_INPUTS;

/// E4 バケット構成。`nb` はバケット数 (2×2=4 / 3×3=9)、`king_bucketed` は玉の
/// base index をバケット化するか (D2)。学習 net と engine で一致必須。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct E4Config {
    /// バケット数。4 (2×2: attacked/defended を各 {0,≥1}) または 9 (3×3: 各 {0,1,2})。
    pub nb: usize,
    /// 玉の base index をバケット化するか (false=玉は bucket 0 固定)。
    pub king_bucketed: bool,
}

impl E4Config {
    /// 2×2 (NB=4)、玉バケット化なし (D2b) — MVP 既定。
    pub const E4_2X2_KINGFIXED: E4Config = E4Config {
        nb: 4,
        king_bucketed: false,
    };
    /// 2×2 (NB=4)、玉バケット化あり (D2a)。
    pub const E4_2X2_KINGBUCKETED: E4Config = E4Config {
        nb: 4,
        king_bucketed: true,
    };
    /// KPE9 (NB=9)、玉バケット化なし。
    pub const KPE9_KINGFIXED: E4Config = E4Config {
        nb: 9,
        king_bucketed: false,
    };
    /// KPE9 (NB=9)、玉バケット化あり。
    pub const KPE9_KINGBUCKETED: E4Config = E4Config {
        nb: 9,
        king_bucketed: true,
    };

    /// 特徴次元数 = base(73,305) × NB。
    #[inline]
    pub const fn dimensions(&self) -> usize {
        45 * PIECE_INPUTS * self.nb
    }
}

/// packed BonaPiece 域 (pack_bonapiece 後、0..1629) の境界。
/// 手駒 [0,90) / 盤上非王駒 [90,1548) / 玉 [1548,1629)。
const PACKED_HAND_END: usize = 90;
const PACKED_BOARD_END: usize = 1548;

/// この packed BonaPiece をバケット化するか。手駒は常に false、盤上非王駒は常に true、
/// 玉は config 依存。両 repo が pack_bonapiece 後の同一域判定で一致させる (D4)。
#[inline]
pub fn packed_is_bucketed(packed_bp: usize, king_bucketed: bool) -> bool {
    if packed_bp < PACKED_HAND_END {
        false // 手駒: マス無し
    } else if packed_bp < PACKED_BOARD_END {
        true // 盤上非王駒
    } else {
        king_bucketed // 玉
    }
}

/// 被攻撃数×被防御数からバケット値を量子化する (0..NB)。
/// NB=4: 各 min(_,1) の 2×2。NB=9: 各 min(_,2) の 3×3。bucket = defended*side + attacked。
#[inline]
pub fn e4_bucket(attacked: u8, defended: u8, nb: usize) -> usize {
    match nb {
        4 => (defended.min(1) as usize) * 2 + (attacked.min(1) as usize),
        9 => (defended.min(2) as usize) * 3 + (attacked.min(2) as usize),
        _ => unreachable!("unsupported E4 NB: {nb}"),
    }
}

/// base index と bucket から E4 index を合成する (`base_index * NB + bucket`)。
#[inline]
pub fn e4_index(base_index: usize, bucket: usize, nb: usize) -> usize {
    base_index * nb + bucket
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_match_config() {
        assert_eq!(E4Config::E4_2X2_KINGFIXED.dimensions(), 73_305 * 4);
        assert_eq!(E4Config::KPE9_KINGFIXED.dimensions(), 73_305 * 9);
    }

    #[test]
    fn bucketed_predicate_domains() {
        // 手駒は常に非バケット
        assert!(!packed_is_bucketed(0, true));
        assert!(!packed_is_bucketed(89, true));
        // 盤上非王駒は常にバケット
        assert!(packed_is_bucketed(90, false));
        assert!(packed_is_bucketed(1547, false));
        // 玉は config 依存
        assert!(!packed_is_bucketed(1548, false));
        assert!(packed_is_bucketed(1548, true));
        assert!(!packed_is_bucketed(1628, false));
    }

    #[test]
    fn bucket_2x2_quantization() {
        assert_eq!(e4_bucket(0, 0, 4), 0);
        assert_eq!(e4_bucket(1, 0, 4), 1); // attacked のみ
        assert_eq!(e4_bucket(0, 1, 4), 2); // defended のみ
        assert_eq!(e4_bucket(3, 5, 4), 3); // 両方 clip→1
    }

    #[test]
    fn bucket_kpe9_quantization() {
        assert_eq!(e4_bucket(0, 0, 9), 0);
        assert_eq!(e4_bucket(2, 0, 9), 2);
        assert_eq!(e4_bucket(0, 2, 9), 6);
        assert_eq!(e4_bucket(5, 5, 9), 8); // 両方 clip→2
    }

    #[test]
    fn e4_index_injective_within_base() {
        // 同一 base で bucket 違いは NB 幅内で単射
        assert_eq!(e4_index(10, 0, 4), 40);
        assert_eq!(e4_index(10, 3, 4), 43);
        assert_eq!(e4_index(11, 0, 4), 44); // 次 base と衝突しない
    }
}
