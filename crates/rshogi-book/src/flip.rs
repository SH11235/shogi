//! FlippedBook 用の局面 / 指し手反転。
//!
//! 「盤 180 度回転 + 先後入替 + 持ち駒入替 + 手番反転」を実装する。片側の手番の局面
//! しか収録しない定跡(片側正規化済み定跡)では、この反転検索が前提となる。
//!
//! **注意**: tools の `mirror_horizontal` / `canonicalize_4t_with_mirror` は左右鏡像であり、
//! ここで実装する 180 度回転(先後反転)とは別物。
//!
//! - 局面: 元局面の SFEN を反転した「生 SFEN」を作り、`Position` で再パース → `to_sfen()` で
//!   rshogi 正準形に直したものを検索キーとする(持ち駒の並び順など正準化を保証するため)。
//! - 指し手: USI 文字列のマスを 180 度回転させる。ply は不変。

use rshogi_core::position::Position;

/// 英大文字 ⇔ 英小文字を入れ替える(先後の入替)。それ以外の文字はそのまま。
fn swap_case(c: char) -> char {
    if c.is_ascii_uppercase() {
        c.to_ascii_lowercase()
    } else if c.is_ascii_lowercase() {
        c.to_ascii_uppercase()
    } else {
        c
    }
}

/// SFEN 盤面部を 180 度回転 + 先後入替する。
///
/// 段の順序を反転し、各段内の駒トークン列も反転して、駒文字の大小を入れ替える。
/// `+P` のような成り駒は `+` を保ったまま駒文字だけ入替。空マス数字はそのまま。
fn flip_board(board: &str) -> Option<String> {
    let ranks: Vec<&str> = board.split('/').collect();
    if ranks.len() != 9 {
        return None;
    }

    let mut out_ranks: Vec<String> = Vec::with_capacity(9);
    for rank in ranks.iter().rev() {
        // 段をトークン(数字 / 駒 / 成り駒)に分解する。
        let mut units: Vec<String> = Vec::new();
        let mut chars = rank.chars();
        while let Some(c) = chars.next() {
            if c == '+' {
                let p = chars.next()?; // 成りマーカーの直後は必ず駒文字
                if !p.is_ascii_alphabetic() {
                    return None;
                }
                units.push(format!("+{}", swap_case(p)));
            } else if c.is_ascii_digit() {
                units.push(c.to_string());
            } else if c.is_ascii_alphabetic() {
                units.push(swap_case(c).to_string());
            } else {
                return None;
            }
        }
        units.reverse();
        out_ranks.push(units.concat());
    }
    Some(out_ranks.join("/"))
}

/// SFEN 持ち駒部を先後入替する(駒文字の大小入替。順序は問わない)。
fn flip_hands(hands: &str) -> String {
    if hands == "-" {
        return "-".to_string();
    }
    hands
        .chars()
        .map(|c| {
            if c.is_ascii_alphabetic() {
                swap_case(c)
            } else {
                c
            }
        })
        .collect()
}

/// SFEN 文字列を反転した「生 SFEN 文字列」に変換する(正準化前)。
/// ply(末尾手数)は不変。手番 `b`⇔`w` を入替。
fn flip_sfen_raw(sfen: &str) -> Option<String> {
    let mut parts = sfen.split_whitespace();
    let board = parts.next()?;
    let turn = parts.next()?;
    let hands = parts.next()?;
    let ply = parts.next();

    let flipped_board = flip_board(board)?;
    let flipped_turn = match turn {
        "b" => "w",
        "w" => "b",
        _ => return None,
    };
    let flipped_hands = flip_hands(hands);

    let mut out = format!("{flipped_board} {flipped_turn} {flipped_hands}");
    if let Some(p) = ply {
        out.push(' ');
        out.push_str(p);
    }
    Some(out)
}

/// SFEN を反転した rshogi 正準形の検索キーを返す。
///
/// 生反転 SFEN を `Position::set_sfen` で再パースし `to_sfen()` で正準化することで、
/// 持ち駒の並び順まで含めて元定跡ファイルのキー空間と一致させる。
pub fn flipped_key(sfen: &str) -> Option<String> {
    let raw = flip_sfen_raw(sfen)?;
    let mut pos = Position::new();
    pos.set_sfen(&raw).ok()?;
    Some(pos.to_sfen())
}

/// USI マス文字列(`"7g"` 等)を 180 度回転する。file: `10-f`、rank: `10-(letter)`。
fn flip_square(sq: &str) -> Option<String> {
    let bytes = sq.as_bytes();
    if bytes.len() != 2 {
        return None;
    }
    let file = bytes[0];
    let rank = bytes[1];
    if !(b'1'..=b'9').contains(&file) || !(b'a'..=b'i').contains(&rank) {
        return None;
    }
    let new_file = b'1' + (b'9' - file); // 10 - file
    let new_rank = b'a' + (b'i' - rank); // 10 - rank
    Some(format!("{}{}", new_file as char, new_rank as char))
}

/// USI 指し手文字列を 180 度回転する。
///
/// - 駒打ち `P*5e`: 駒種はそのまま、打つマスを回転。
/// - 通常手 `7g7f` / 成り `7g7f+`: from/to を回転、成りフラグは保持。
///
/// パースできない入力は `None`。
pub fn flip_usi_move(usi: &str) -> Option<String> {
    if let Some((piece, dst)) = usi.split_once('*') {
        // 駒打ち。
        if piece.len() != 1 {
            return None;
        }
        let flipped_dst = flip_square(dst)?;
        return Some(format!("{piece}*{flipped_dst}"));
    }

    let bytes = usi.as_bytes();
    if bytes.len() < 4 {
        return None;
    }
    let from = flip_square(&usi[0..2])?;
    let to = flip_square(&usi[2..4])?;
    let promote = if bytes.len() == 5 && bytes[4] == b'+' {
        "+"
    } else if bytes.len() == 4 {
        ""
    } else {
        return None;
    };
    Some(format!("{from}{to}{promote}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_core::position::Position;

    /// SFEN を rshogi 正準形に直す(比較用)。
    fn canonical(sfen: &str) -> String {
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        pos.to_sfen()
    }

    #[test]
    fn flip_square_rotates_180() {
        assert_eq!(flip_square("7g").as_deref(), Some("3c"));
        assert_eq!(flip_square("5e").as_deref(), Some("5e")); // 中央は不動
        assert_eq!(flip_square("1a").as_deref(), Some("9i"));
        assert_eq!(flip_square("9i").as_deref(), Some("1a"));
    }

    #[test]
    fn flip_usi_move_normal_and_promote_and_drop() {
        assert_eq!(flip_usi_move("7g7f").as_deref(), Some("3c3d"));
        assert_eq!(flip_usi_move("7g7f+").as_deref(), Some("3c3d+"));
        assert_eq!(flip_usi_move("P*5e").as_deref(), Some("P*5e"));
        assert_eq!(flip_usi_move("8h2b+").as_deref(), Some("2b8h+"));
    }

    #[test]
    fn flip_usi_move_is_involutive() {
        for m in ["7g7f", "7g7f+", "P*5e", "8h2b+", "1a1b", "N*3c"] {
            let once = flip_usi_move(m).unwrap();
            let twice = flip_usi_move(&once).unwrap();
            assert_eq!(twice, m, "flip(flip({m})) should equal {m}");
        }
    }

    #[test]
    fn hirate_flip_swaps_only_turn() {
        // 平手初期局面の盤面・持ち駒は 180 度回転 + 先後入替で不変。手番のみ b→w。
        let hirate = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let hirate_white = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1";
        assert_eq!(flipped_key(hirate).as_deref(), Some(hirate_white));
        // 往復で元(正準形)に戻る。
        assert_eq!(flipped_key(hirate_white).as_deref(), Some(canonical(hirate).as_str()));
    }

    #[test]
    fn flipped_key_swaps_turn_and_side() {
        // 7g7f 後(後手番)を反転すると、対称なので先手番の同一局面になる。
        let after_76 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2";
        let flipped = flipped_key(after_76).unwrap();
        // 反転後は手番 b、盤面は 3c3d を突いた先手側の鏡。
        assert!(flipped.contains(" b "), "flipped turn should be black: {flipped}");
        // 往復で元(正準形)に戻る。
        assert_eq!(flipped_key(&flipped).as_deref(), Some(canonical(after_76).as_str()));
    }

    #[test]
    fn flip_round_trip_with_hands() {
        // 持ち駒あり・成り駒あり・非対称局面での往復整合。
        let sfen = "l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w GR5pnsg 1";
        let canon = canonical(sfen);
        let once = flipped_key(&canon).unwrap();
        let twice = flipped_key(&once).unwrap();
        assert_eq!(twice, canon, "flip(flip(pos)) should equal pos");
        // 反転で手番が入れ替わる。
        assert!(canon.contains(" w "));
        assert!(once.contains(" b "));
    }

    #[test]
    fn flip_move_matches_flipped_position_legality() {
        // 反転した指し手が反転した局面で合法になることを確認する(整合テスト)。
        use rshogi_core::types::Move;
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let flipped = flipped_key(sfen).unwrap();
        let mut fpos = Position::new();
        fpos.set_sfen(&flipped).unwrap();

        let orig_move = "7g7f";
        let flipped_move = flip_usi_move(orig_move).unwrap();
        let mv = Move::from_usi(&flipped_move).unwrap();
        let mv = fpos.to_move(mv).expect("flipped move decodes");
        assert!(fpos.pseudo_legal(mv) && fpos.is_legal(mv));
    }
}
