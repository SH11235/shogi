//! HalfKa-E4 特徴列挙 (full recompute)
//!
//! base HalfKaHmMerged の active index 列挙を、各駒マスの被攻撃×被防御バケットで
//! 拡張する。`e4_index = base_index * NB + bucket` (`bona_piece_halfka_e4`)。
//! bucket は `pos.board_effect` の per-square count から求めるため、base の Feature
//! trait (pos 非受領) でなく pos を取る専用関数として実装する (threat と同型)。
//!
//! 差分更新 `append_changed_e4_indices` は本モジュールに後続で追加する。full recompute
//! (本関数) は差分の正当性検証 (決定論 verify) の ground truth。

use super::accumulator::{IndexList, MAX_ACTIVE_FEATURES};
use super::bona_piece::BonaPiece;
use super::bona_piece_halfka_e4::{E4Config, e4_bucket, e4_index, packed_is_bucketed};
use super::bona_piece_halfka_hm_merged::{halfka_index, is_hm_mirror, king_bucket, pack_bonapiece};
use super::piece_list::PieceNumber;
use super::threat_features::decode_board_square_fb;
use crate::position::Position;
use crate::types::Color;

/// perspective 視点の E4 active index を全て列挙する (full recompute)。
///
/// 各 PieceList slot について base index を求め、bucketed な駒 (盤上非王駒 / 玉は
/// config 依存) は物理マスの被攻撃×被防御 count からバケットを付ける。手駒と
/// 非バケット駒は bucket 0。`pos.board_effect` を参照するため board_effects が
/// 最新 (非 dirty) であることが前提。
pub fn append_active_e4(
    pos: &Position,
    config: E4Config,
    perspective: Color,
    active: &mut IndexList<MAX_ACTIVE_FEATURES>,
) {
    let king_sq = pos.king_square(perspective);
    let kb = king_bucket(king_sq, perspective);
    let hm_mirror = is_hm_mirror(king_sq, perspective);

    let pieces_persp = if perspective == Color::Black {
        pos.piece_list().piece_list_fb()
    } else {
        pos.piece_list().piece_list_fw()
    };
    // 物理マスは常に fb (先手視点=物理座標) から取る。fw[i] と fb[i] は同一 PieceNumber。
    let pieces_fb = pos.piece_list().piece_list_fb();

    for i in 0..PieceNumber::NB {
        let bp = pieces_persp[i];
        if bp == BonaPiece::ZERO {
            continue;
        }
        let packed = pack_bonapiece(bp, hm_mirror);
        let base_index = halfka_index(kb, packed);
        let bucket = if packed_is_bucketed(packed, config.king_bucketed) {
            // bucketed 駒は必ず盤上に物理マスを持つ。
            let sq = decode_board_square_fb(pieces_fb[i])
                .expect("bucketed piece must have a board square");
            let c = pos.piece_on(sq).color();
            let attacked = pos.board_effect(!c, sq);
            let defended = pos.board_effect(c, sq);
            e4_bucket(attacked, defended, config.nb)
        } else {
            0
        };
        let _ = active.push(e4_index(base_index, bucket, config.nb));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::features::{Feature, HalfKaHmMerged};

    fn base_active(pos: &Position, persp: Color) -> Vec<usize> {
        let mut list = IndexList::<MAX_ACTIVE_FEATURES>::new();
        HalfKaHmMerged::append_active_indices(pos, persp, &mut list);
        let mut v: Vec<usize> = list.iter().collect();
        v.sort_unstable();
        v
    }

    /// E4 の active index を base で割ると base HalfKaHmMerged の active 集合に一致し、
    /// 余りが有効な bucket に収まることを、検証済みの base 実装と突き合わせて確認する。
    #[test]
    fn e4_active_divides_back_to_base_set() {
        let sfens = [
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1",
            "6n1l/2+S1k4/2lp4p/1np1B2b1/3PP4/1N1S3rP/1P2+pPP+p1/1p1G5/3KG2r1 b GSN2L4Pgs2p 1",
        ];
        for cfg in [
            E4Config::E4_2X2_KINGFIXED,
            E4Config::E4_2X2_KINGBUCKETED,
            E4Config::KPE9_KINGFIXED,
            E4Config::KPE9_KINGBUCKETED,
        ] {
            for sfen in sfens {
                let mut pos = Position::new();
                pos.set_sfen(sfen).unwrap();
                pos.recompute_board_effects();
                for persp in [Color::Black, Color::White] {
                    let base = base_active(&pos, persp);
                    let mut e4 = IndexList::<MAX_ACTIVE_FEATURES>::new();
                    append_active_e4(&pos, cfg, persp, &mut e4);
                    assert_eq!(e4.len(), base.len(), "active 数一致 {sfen} {persp:?}");
                    let mut recovered: Vec<usize> = e4
                        .iter()
                        .map(|idx| {
                            let bucket = idx % cfg.nb;
                            assert!(bucket < cfg.nb, "bucket 範囲内");
                            idx / cfg.nb
                        })
                        .collect();
                    recovered.sort_unstable();
                    assert_eq!(recovered, base, "e4/NB == base 集合 {sfen} {persp:?} {cfg:?}");
                }
            }
        }
    }
}
