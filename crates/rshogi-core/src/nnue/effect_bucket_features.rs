//! HalfKaHmMerged + EffectBucket 特徴列挙 (full recompute)
//!
//! base HalfKaHmMerged の active index 列挙を、各駒マスの被攻撃×被防御バケットで
//! 拡張する。`effect_bucket_index = base_index * NB + bucket` (`bona_piece_effect_bucket`)。
//! bucket は `pos.board_effect` の per-square count から求めるため、base の Feature
//! trait (pos 非受領) でなく pos を取る専用関数として実装する (threat と同型)。
//!
//! 差分更新 `append_changed_effect_bucket_indices` は本モジュールに後続で追加する。full recompute
//! (本関数) は差分の正当性検証 (決定論 verify) の ground truth。

use super::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES};
use super::bona_piece::BonaPiece;
use super::bona_piece_effect_bucket::{
    EffectBucketConfig, effect_bucket, effect_bucket_index, packed_is_bucketed,
};
use super::bona_piece_halfka_hm_merged::{
    E_KING, F_KING, FE_HAND_END, FE_OLD_END, halfka_index, is_hm_mirror, king_bucket,
    pack_bonapiece,
};
use super::piece_list::PieceNumber;
use crate::position::{BoardEffects, Position};
use crate::types::{Color, Square};

/// perspective 視点の effect bucket active index を全て列挙する (full recompute)。
///
/// 各 PieceList slot について base index を求め、bucketed な駒 (盤上非王駒 / 玉は
/// config 依存) は物理マスの被攻撃×被防御 count からバケットを付ける。手駒と
/// 非バケット駒は bucket 0。`pos.board_effect` を参照するため board_effects が
/// 最新 (非 dirty) であることが前提。
pub fn append_active_effect_bucket(
    pos: &Position,
    config: EffectBucketConfig,
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
            effect_bucket(attacked, defended, config.nb)
        } else {
            0
        };
        let _ = active.push(effect_bucket_index(base_index, bucket, config.nb));
    }
}

/// do_move 後の局面と do_move 前の利きスナップショットから、effect bucket active index の差分を列挙する。
///
/// `perspective` 側の玉が動いた場合、king bucket が変わって全 base index が移動し得るため
/// `false` を返す。呼び出し側は full refresh にフォールバックすること。
/// `pos.board_effects()` は do_move 後の状態で再計算済みであることが前提。
pub fn append_changed_effect_bucket_indices(
    pos: &Position,
    prev_effects: &BoardEffects,
    dirty_piece: &DirtyPiece,
    config: EffectBucketConfig,
    perspective: Color,
    king_sq: Square,
    removed: &mut IndexList<MAX_CHANGED_FEATURES>,
    added: &mut IndexList<MAX_CHANGED_FEATURES>,
) -> bool {
    if dirty_piece.king_moved[perspective.index()] {
        return false;
    }

    let kb = king_bucket(king_sq, perspective);
    let hm_mirror = is_hm_mirror(king_sq, perspective);
    let mut dirty_after_squares: [Option<Square>; 2] = [None, None];

    for (i, dirty_after_sq) in
        dirty_after_squares.iter_mut().enumerate().take(dirty_piece.dirty_num as usize)
    {
        let cp = &dirty_piece.changed_piece[i];
        *dirty_after_sq = decode_board_square_fb(cp.new_piece.fb);

        let old_bp = if perspective == Color::Black {
            cp.old_piece.fb
        } else {
            cp.old_piece.fw
        };
        if old_bp != BonaPiece::ZERO {
            let packed = pack_bonapiece(old_bp, hm_mirror);
            let bucket = bucket_for_bonapiece_before(cp.old_piece.fb, packed, prev_effects, config);
            if !removed.push(effect_bucket_index(halfka_index(kb, packed), bucket, config.nb)) {
                return false;
            }
        }

        let new_bp = if perspective == Color::Black {
            cp.new_piece.fb
        } else {
            cp.new_piece.fw
        };
        if new_bp != BonaPiece::ZERO {
            let packed = pack_bonapiece(new_bp, hm_mirror);
            let bucket = bucket_for_bonapiece_after(pos, cp.new_piece.fb, packed, config);
            if !added.push(effect_bucket_index(halfka_index(kb, packed), bucket, config.nb)) {
                return false;
            }
        }
    }

    for sq_raw in 0..Square::NUM {
        let sq = Square::from_u8(sq_raw as u8).expect("0..Square::NUM is a valid square");
        if dirty_after_squares.iter().flatten().any(|dirty_sq| *dirty_sq == sq) {
            continue;
        }

        let pc = pos.piece_on(sq);
        if pc.is_none() {
            continue;
        }
        let color = pc.color();
        let old_bucket = effect_bucket(
            prev_effects.effect(!color, sq),
            prev_effects.effect(color, sq),
            config.nb,
        );
        let new_bucket =
            effect_bucket(pos.board_effect(!color, sq), pos.board_effect(color, sq), config.nb);
        if old_bucket == new_bucket {
            continue;
        }

        let Some(bp) = bonapiece_at_square(pos, perspective, sq) else {
            continue;
        };
        let packed = pack_bonapiece(bp, hm_mirror);
        if !packed_is_bucketed(packed, config.king_bucketed) {
            continue;
        }
        let base = halfka_index(kb, packed);
        if !removed.push(effect_bucket_index(base, old_bucket, config.nb)) {
            return false;
        }
        if !added.push(effect_bucket_index(base, new_bucket, config.nb)) {
            return false;
        }
    }

    true
}

fn bucket_for_bonapiece_before(
    bp_fb: BonaPiece,
    packed: usize,
    prev_effects: &BoardEffects,
    config: EffectBucketConfig,
) -> usize {
    if !packed_is_bucketed(packed, config.king_bucketed) {
        return 0;
    }
    let sq = decode_board_square_fb(bp_fb).expect("bucketed piece must have a board square");
    let color = decode_board_color_fb(bp_fb).expect("bucketed piece must have a color");
    effect_bucket(prev_effects.effect(!color, sq), prev_effects.effect(color, sq), config.nb)
}

fn bucket_for_bonapiece_after(
    pos: &Position,
    bp_fb: BonaPiece,
    packed: usize,
    config: EffectBucketConfig,
) -> usize {
    if !packed_is_bucketed(packed, config.king_bucketed) {
        return 0;
    }
    let sq = decode_board_square_fb(bp_fb).expect("bucketed piece must have a board square");
    let color = pos.piece_on(sq).color();
    effect_bucket(pos.board_effect(!color, sq), pos.board_effect(color, sq), config.nb)
}

fn bonapiece_at_square(pos: &Position, perspective: Color, sq: Square) -> Option<BonaPiece> {
    let pieces_fb = pos.piece_list().piece_list_fb();
    let pieces_persp = if perspective == Color::Black {
        pos.piece_list().piece_list_fb()
    } else {
        pos.piece_list().piece_list_fw()
    };

    for i in 0..PieceNumber::NB {
        if decode_board_square_fb(pieces_fb[i]) == Some(sq) {
            return Some(pieces_persp[i]);
        }
    }
    None
}

fn decode_board_square_fb(bp: BonaPiece) -> Option<Square> {
    let v = bp.value() as usize;
    if v == 0 {
        return None;
    }
    if (FE_HAND_END..FE_OLD_END).contains(&v) {
        return Square::from_u8(((v - FE_HAND_END) % Square::NUM) as u8);
    }
    if (F_KING..F_KING + Square::NUM).contains(&v) {
        return Square::from_u8((v - F_KING) as u8);
    }
    if (E_KING..E_KING + Square::NUM).contains(&v) {
        return Square::from_u8((v - E_KING) as u8);
    }
    None
}

fn decode_board_color_fb(bp: BonaPiece) -> Option<Color> {
    let v = bp.value() as usize;
    if (FE_HAND_END..FE_OLD_END).contains(&v) {
        let piece_plane = (v - FE_HAND_END) / Square::NUM;
        return Some(if piece_plane.is_multiple_of(2) {
            Color::Black
        } else {
            Color::White
        });
    }
    if (F_KING..F_KING + Square::NUM).contains(&v) {
        return Some(Color::Black);
    }
    if (E_KING..E_KING + Square::NUM).contains(&v) {
        return Some(Color::White);
    }
    None
}

/// SFEN 局面の effect bucket active index を sorted で返す (形式一致 golden 用)。
/// board_effects を再計算してから full recompute 列挙する。
pub fn effect_bucket_active_indices_for_sfen(
    sfen: &str,
    config: EffectBucketConfig,
    perspective: Color,
) -> Result<Vec<usize>, crate::position::SfenError> {
    let mut pos = Position::new();
    pos.set_sfen(sfen)?;
    pos.recompute_board_effects();
    let mut list = IndexList::<MAX_ACTIVE_FEATURES>::new();
    append_active_effect_bucket(&pos, config, perspective, &mut list);
    let mut v: Vec<usize> = list.iter().collect();
    v.sort_unstable();
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movegen::{MoveList, generate_legal_all};
    use crate::nnue::features::{Feature, HalfKaHmMerged};
    use crate::types::Move;

    fn base_active(pos: &Position, persp: Color) -> Vec<usize> {
        let mut list = IndexList::<MAX_ACTIVE_FEATURES>::new();
        HalfKaHmMerged::append_active_indices(pos, persp, &mut list);
        let mut v: Vec<usize> = list.iter().collect();
        v.sort_unstable();
        v
    }

    /// effect bucket の active index を base で割ると base HalfKaHmMerged の active 集合に一致し、
    /// 余りが有効な bucket に収まることを、検証済みの base 実装と突き合わせて確認する。
    #[test]
    fn effect_bucket_active_divides_back_to_base_set() {
        let sfens = [
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1",
            "6n1l/2+S1k4/2lp4p/1np1B2b1/3PP4/1N1S3rP/1P2+pPP+p1/1p1G5/3KG2r1 b GSN2L4Pgs2p 1",
        ];
        for cfg in [
            EffectBucketConfig::KINGFIXED_2X2,
            EffectBucketConfig::KINGBUCKETED_2X2,
            EffectBucketConfig::KINGFIXED_3X3,
            EffectBucketConfig::KINGBUCKETED_3X3,
        ] {
            for sfen in sfens {
                let mut pos = Position::new();
                pos.set_sfen(sfen).unwrap();
                pos.recompute_board_effects();
                for persp in [Color::Black, Color::White] {
                    let base = base_active(&pos, persp);
                    let mut effect_bucket_indices = IndexList::<MAX_ACTIVE_FEATURES>::new();
                    append_active_effect_bucket(&pos, cfg, persp, &mut effect_bucket_indices);
                    assert_eq!(
                        effect_bucket_indices.len(),
                        base.len(),
                        "active 数一致 {sfen} {persp:?}"
                    );
                    let mut recovered: Vec<usize> = effect_bucket_indices
                        .iter()
                        .map(|idx| {
                            let bucket = idx % cfg.nb;
                            assert!(bucket < cfg.nb, "bucket 範囲内");
                            idx / cfg.nb
                        })
                        .collect();
                    recovered.sort_unstable();
                    assert_eq!(
                        recovered, base,
                        "effect_bucket/NB == base 集合 {sfen} {persp:?} {cfg:?}"
                    );
                }
            }
        }
    }

    fn effect_bucket_active(pos: &Position, cfg: EffectBucketConfig, persp: Color) -> Vec<usize> {
        let mut list = IndexList::<MAX_ACTIVE_FEATURES>::new();
        append_active_effect_bucket(pos, cfg, persp, &mut list);
        let mut v: Vec<usize> = list.iter().collect();
        v.sort_unstable();
        v
    }

    fn apply_changed(
        mut active: Vec<usize>,
        removed: &IndexList<MAX_CHANGED_FEATURES>,
        added: &IndexList<MAX_CHANGED_FEATURES>,
        context: &str,
    ) -> Vec<usize> {
        active.sort_unstable();
        for idx in removed.iter() {
            let pos = active
                .binary_search(&idx)
                .unwrap_or_else(|_| panic!("removed index not active: {idx} ({context})"));
            active.remove(pos);
        }
        active.extend(added.iter());
        active.sort_unstable();
        active
    }

    #[test]
    fn effect_bucket_changed_matches_active_all_legal_moves() {
        let sfens = [
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1",
            "6n1l/2+S1k4/2lp4p/1np1B2b1/3PP4/1N1S3rP/1P2+pPP+p1/1p1G5/3KG2r1 b GSN2L4Pgs2p 1",
        ];

        for cfg in [
            EffectBucketConfig::KINGFIXED_2X2,
            EffectBucketConfig::KINGBUCKETED_2X2,
            EffectBucketConfig::KINGFIXED_3X3,
            EffectBucketConfig::KINGBUCKETED_3X3,
        ] {
            for sfen in sfens {
                let mut pos = Position::new();
                pos.set_sfen(sfen).unwrap();
                pos.recompute_board_effects();

                let mut moves = MoveList::new();
                generate_legal_all(&pos, &mut moves);
                let moves: Vec<Move> = moves.iter().copied().collect();

                for m in moves {
                    let prev_effects = pos.board_effects().clone();
                    let active_before = [
                        effect_bucket_active(&pos, cfg, Color::Black),
                        effect_bucket_active(&pos, cfg, Color::White),
                    ];
                    let gives_check = pos.gives_check(m);
                    let dirty_piece = pos.do_move(m, gives_check);
                    pos.recompute_board_effects();

                    for perspective in [Color::Black, Color::White] {
                        let active_after = effect_bucket_active(&pos, cfg, perspective);
                        let mut removed = IndexList::<MAX_CHANGED_FEATURES>::new();
                        let mut added = IndexList::<MAX_CHANGED_FEATURES>::new();
                        let ok = append_changed_effect_bucket_indices(
                            &pos,
                            &prev_effects,
                            &dirty_piece,
                            cfg,
                            perspective,
                            pos.king_square(perspective),
                            &mut removed,
                            &mut added,
                        );
                        if dirty_piece.king_moved[perspective.index()] {
                            assert!(!ok, "king moved must request refresh: {sfen} {m:?} {cfg:?}");
                            continue;
                        }
                        assert!(
                            ok,
                            "changed effect bucket overflow: {sfen} {m:?} {perspective:?} {cfg:?}"
                        );
                        let context = format!("{sfen} {m:?} {perspective:?} {cfg:?}");
                        let updated = apply_changed(
                            active_before[perspective.index()].clone(),
                            &removed,
                            &added,
                            &context,
                        );
                        assert_eq!(updated, active_after, "{context}");
                    }

                    pos.undo_move(m);
                    pos.recompute_board_effects();
                }
            }
        }
    }
}
