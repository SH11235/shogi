//! cutoff_cnt の探索開始時クリアの回帰テスト

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::eval::EvalHash;
use crate::position::Position;
use crate::search::alpha_beta::SearchWorker;
use crate::search::engine::search_helper;
use crate::search::{LimitsType, SearchTuneParams, TimeManagement};
use crate::tt::TranspositionTable;

#[test]
fn iterative_deepening_clears_low_cutoff_counts_for_each_search() {
    const STACK_SIZE: usize = 64 * 1024 * 1024;

    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(|| {
            let tt = Arc::new(TranspositionTable::new(16));
            let eval_hash = Arc::new(EvalHash::new(1));
            let mut worker = SearchWorker::new(tt, eval_hash, 0, 0, SearchTuneParams::default());
            let mut pos = Position::new();
            pos.set_hirate();
            let limits = LimitsType::new();
            let increase_depth = AtomicBool::new(true);

            for dirty_value in [3, 7] {
                worker.state.stack[0].cutoff_cnt = dirty_value;
                worker.state.stack[1].cutoff_cnt = dirty_value;
                let mut time_manager = TimeManagement::new(
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(AtomicBool::new(false)),
                );

                search_helper(
                    &mut worker,
                    &mut pos,
                    &limits,
                    &mut time_manager,
                    0,
                    false,
                    None,
                    &increase_depth,
                );

                assert_eq!(worker.state.stack[0].cutoff_cnt, 0);
                assert_eq!(worker.state.stack[1].cutoff_cnt, 0);
            }
        })
        .expect("failed to spawn test thread with large stack")
        .join()
        .expect("test thread panicked");
}
