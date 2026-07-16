//! alpha_beta モジュールのテスト

use std::sync::Arc;

use crate::eval::EvalHash;
use crate::search::alpha_beta::{
    SearchWorker, build_reductions, enforce_decreasing_depth, reduction,
    should_activate_depth_liveness,
};
use crate::search::{LimitsType, SearchTuneParams};
use crate::tt::TranspositionTable;

#[test]
fn test_reduction_values() {
    // reduction(true, 10, 5) などが正の値を返すことを確認
    let tune = SearchTuneParams::default();
    let reductions = build_reductions(tune.lmr_table_coeff);
    let root_delta = 64;
    let delta = 32;
    assert!(reduction(&reductions, &tune, true, 10, 5, delta, root_delta) / 1024 >= 0);
    assert!(
        reduction(&reductions, &tune, false, 10, 5, delta, root_delta) / 1024
            >= reduction(&reductions, &tune, true, 10, 5, delta, root_delta) / 1024
    );
}

#[test]
fn test_reduction_bounds() {
    // 境界値テスト
    let tune = SearchTuneParams::default();
    let reductions = build_reductions(tune.lmr_table_coeff);
    let root_delta = 64;
    let delta = 32;
    assert_eq!(reduction(&reductions, &tune, true, 0, 0, delta, root_delta), 0); // depth=0, mc=0 は計算外
    assert!(reduction(&reductions, &tune, true, 63, 63, delta, root_delta) / 1024 < 64);
    assert!(reduction(&reductions, &tune, false, 63, 63, delta, root_delta) / 1024 < 64);
}

/// depth/move_countが大きい場合にreductionが正の値を返すことを確認
#[test]
fn test_reduction_returns_nonzero_for_large_values() {
    let tune = SearchTuneParams::default();
    let reductions = build_reductions(tune.lmr_table_coeff);
    let root_delta = 64;
    let delta = 32;
    // 深い探索で多くの手を試した場合、reductionは正の値であるべき
    let r = reduction(&reductions, &tune, false, 10, 10, delta, root_delta) / 1024;
    assert!(
        r > 0,
        "reduction should return positive value for depth=10, move_count=10, got {r}"
    );

    // improving=trueの場合は若干小さい値になる
    let r_imp = reduction(&reductions, &tune, true, 10, 10, delta, root_delta) / 1024;
    assert!(r >= r_imp, "non-improving should have >= reduction than improving");
}

/// 境界ケース: depth=1, move_count=1でもreduction関数が動作することを確認
#[test]
fn test_reduction_small_values() {
    let tune = SearchTuneParams::default();
    let reductions = build_reductions(tune.lmr_table_coeff);
    let root_delta = 64;
    let delta = 32;
    // 小さな値でもpanicしないことを確認
    let r = reduction(&reductions, &tune, true, 1, 1, delta, root_delta) / 1024;
    assert!(r >= 0, "reduction should not be negative");
}

#[test]
fn test_reduction_extremes_no_overflow() {
    let tune = SearchTuneParams::default();
    let reductions = build_reductions(tune.lmr_table_coeff);
    // 最大depth/mcでもオーバーフローせずに値が得られることを確認
    let delta = 0;
    let root_delta = 1;
    let r = reduction(&reductions, &tune, false, 63, 63, delta, root_delta);
    assert!(
        (0..i32::MAX / 2).contains(&r),
        "reduction extreme should be in safe range, got {r}"
    );
}

#[test]
fn test_reduction_zero_root_delta_clamped() {
    let tune = SearchTuneParams::default();
    let reductions = build_reductions(tune.lmr_table_coeff);
    // root_delta=0 を渡しても内部で1にクランプされることを確認
    let r = reduction(&reductions, &tune, false, 10, 10, 0, 0) / 1024;
    assert!(r >= 0, "reduction should clamp root_delta to >=1 even when 0 is passed");
}

#[test]
fn test_depth_liveness_activation_requires_node_and_run_thresholds() {
    assert!(!should_activate_depth_liveness(99_999, 0, 8, 100_000, 8));
    assert!(!should_activate_depth_liveness(100_000, 0, 7, 100_000, 8));
    assert!(should_activate_depth_liveness(100_000, 0, 8, 100_000, 8));

    // root search が go の途中から始まっても、試行内の消費 node だけを数える。
    assert!(!should_activate_depth_liveness(149_999, 50_000, 8, 100_000, 8));
    assert!(should_activate_depth_liveness(150_000, 50_000, 8, 100_000, 8));

    // どちらかの閾値が0なら同一バイナリの無効化条件になる。
    assert!(!should_activate_depth_liveness(u64::MAX, 0, 256, 0, 8));
    assert!(!should_activate_depth_liveness(u64::MAX, 0, 256, 100_000, 0));
}

#[test]
fn test_depth_liveness_enforces_strict_progress_only_after_activation() {
    assert_eq!(enforce_decreasing_depth(5, 4, false), 5);
    assert_eq!(enforce_decreasing_depth(4, 4, false), 4);
    assert_eq!(enforce_decreasing_depth(5, 4, true), 3);
    assert_eq!(enforce_decreasing_depth(4, 4, true), 3);
    assert_eq!(enforce_decreasing_depth(2, 4, true), 2);
    assert_eq!(enforce_decreasing_depth(1, 1, true), 0);
}

#[test]
fn test_depth_liveness_state_is_reset_for_each_go() {
    let tt = Arc::new(TranspositionTable::new(16));
    let eval_hash = Arc::new(EvalHash::new(1));
    let mut worker = SearchWorker::new(tt, eval_hash, 0, 0, SearchTuneParams::default());
    let mut fixed_depth = LimitsType::new();
    fixed_depth.depth = 15;

    assert!(!worker.state.depth_liveness_is_enabled());
    worker.prepare_search(&fixed_depth);

    assert!(worker.state.depth_liveness_is_enabled());
    worker.state.set_depth_liveness_active_for_test(true);
    assert!(worker.state.depth_liveness_is_active_for_test());
    worker.prepare_search(&fixed_depth);

    assert!(worker.state.depth_liveness_is_enabled());
    assert!(!worker.state.depth_liveness_is_active_for_test());

    let mut node_limited = fixed_depth;
    node_limited.nodes = 100_000;
    worker.prepare_search(&node_limited);
    assert!(!worker.state.depth_liveness_is_enabled());
}

#[test]
fn test_sentinel_initialization() {
    // SearchWorker作成時にsentinelが正しく初期化されることを確認
    let tt = Arc::new(TranspositionTable::new(16));
    let eval_hash = Arc::new(EvalHash::new(1));
    let worker = SearchWorker::new(tt, eval_hash, 0, 0, SearchTuneParams::default());

    // sentinelポインタがdanglingではなく、実際のテーブルを指していることを確認
    let sentinel = worker.cont_history_sentinel;
    // NonNullはnullにならないことが保証されているので、
    // 代わりにsafeにderefできることを確認（ポインタが有効なメモリを指していること）
    let sentinel_ref = unsafe { sentinel.as_ref() };
    // PieceToHistoryテーブルはYO準拠の初期値(-529)で初期化されているはず
    assert_eq!(
        sentinel_ref.get(crate::types::Piece::B_PAWN, crate::types::Square::SQ_11),
        crate::search::history::CONTINUATION_HISTORY_INIT,
        "sentinel table should be initialized with YO-standard value"
    );

    // 全てのスタックエントリがsentinelで初期化されていることを確認
    for (i, stack) in worker.state.stack.iter().enumerate() {
        assert_eq!(
            stack.cont_history_ptr, sentinel,
            "stack[{i}].cont_history_ptr should be initialized to sentinel"
        );
    }
}
