//! 玉位置に基づく入玉ドメイン分類。

use rshogi_core::position::Position;
use rshogi_core::types::Color;

/// 入玉済みの tier 番号。
pub const ENTERED_TIER: usize = 0;

/// 玉位置から 0=entered、1=advancing、2=normal を返す。
///
/// entered は先手玉の rank が 2 以下、または後手玉の rank が 6 以上の局面。
pub fn classify(pos: &Position) -> usize {
    let black = pos.king_square(Color::Black).rank() as usize;
    let white = pos.king_square(Color::White).rank() as usize;
    if black <= 2 || white >= 6 {
        ENTERED_TIER
    } else if (3..=5).contains(&black) || (3..=5).contains(&white) {
        1
    } else {
        2
    }
}
