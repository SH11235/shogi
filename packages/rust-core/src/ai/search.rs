//! Search engine for shogi
//!
//! Implements alpha-beta search with basic enhancements

use super::board::Position;
use super::evaluate::evaluate;
use super::movegen::MoveGen;
use super::moves::Move;
use std::time::{Duration, Instant};

/// Search limits
#[derive(Clone, Debug)]
pub struct SearchLimits {
    /// Maximum search depth
    pub depth: u8,
    /// Maximum search time
    pub time: Option<Duration>,
    /// Maximum nodes to search
    pub nodes: Option<u64>,
}

impl Default for SearchLimits {
    fn default() -> Self {
        SearchLimits {
            depth: 6,
            time: None,
            nodes: None,
        }
    }
}

/// Search statistics
#[derive(Clone, Debug, Default)]
pub struct SearchStats {
    /// Nodes searched
    pub nodes: u64,
    /// Time elapsed
    pub elapsed: Duration,
    /// Principal variation
    pub pv: Vec<Move>,
}

/// Search result
#[derive(Clone, Debug)]
pub struct SearchResult {
    /// Best move found
    pub best_move: Option<Move>,
    /// Evaluation score (from side to move perspective)
    pub score: i32,
    /// Search statistics
    pub stats: SearchStats,
}

/// Search engine
pub struct Searcher {
    /// Search limits
    limits: SearchLimits,
    /// Start time
    start_time: Instant,
    /// Node counter
    nodes: u64,
    /// Principal variation
    pv: Vec<Vec<Move>>,
}

impl Searcher {
    /// Create new searcher with limits
    pub fn new(limits: SearchLimits) -> Self {
        Searcher {
            limits,
            start_time: Instant::now(),
            nodes: 0,
            pv: vec![vec![]; 128], // Max depth 128
        }
    }

    /// Search position for best move
    pub fn search(&mut self, pos: &Position) -> SearchResult {
        self.start_time = Instant::now();
        self.nodes = 0;

        let mut best_move = None;
        let mut best_score = -30000; // Negative infinity

        // Iterative deepening
        for depth in 1..=self.limits.depth {
            let score = self.alpha_beta(pos, depth, -30000, 30000);

            // Check time limit
            if self.should_stop() {
                break;
            }

            best_score = score;
            if !self.pv[0].is_empty() {
                best_move = Some(self.pv[0][0]);
            }
        }

        SearchResult {
            best_move,
            score: best_score,
            stats: SearchStats {
                nodes: self.nodes,
                elapsed: self.start_time.elapsed(),
                pv: self.pv[0].clone(),
            },
        }
    }

    /// Alpha-beta search
    fn alpha_beta(&mut self, pos: &Position, depth: u8, mut alpha: i32, beta: i32) -> i32 {
        self.nodes += 1;

        // Check limits
        if self.should_stop() {
            return 0;
        }

        // Leaf node - return static evaluation
        if depth == 0 {
            return evaluate(pos);
        }

        // Clear PV for this ply
        let ply = self.limits.depth - depth;
        self.pv[ply as usize].clear();

        // Generate moves
        let mut gen = MoveGen::new(pos);
        let moves = gen.generate_all();

        // No legal moves - checkmate or stalemate
        if moves.is_empty() {
            if gen.checkers.count_ones() > 0 {
                // Checkmate - return negative score
                return -30000 + ply as i32;
            } else {
                // Stalemate (shouldn't happen in shogi)
                return 0;
            }
        }

        let mut best_score = -30000;

        // Search all moves
        for &mv in moves.as_slice() {
            // Make move
            let mut new_pos = pos.clone();
            new_pos.do_move(mv);

            // Recursive search
            let score = -self.alpha_beta(&new_pos, depth - 1, -beta, -alpha);

            // Update best score
            if score > best_score {
                best_score = score;

                // Update alpha
                if score > alpha {
                    alpha = score;

                    // Update principal variation
                    self.pv[ply as usize].clear();
                    self.pv[ply as usize].push(mv);
                    // Copy from next ply's PV
                    let next_ply = (ply + 1) as usize;
                    if next_ply < self.pv.len() {
                        let next_pv = self.pv[next_ply].clone();
                        self.pv[ply as usize].extend_from_slice(&next_pv);
                    }

                    // Beta cutoff
                    if score >= beta {
                        break;
                    }
                }
            }
        }

        best_score
    }

    /// Check if search should stop
    fn should_stop(&self) -> bool {
        // Check node limit
        if let Some(max_nodes) = self.limits.nodes {
            if self.nodes >= max_nodes {
                return true;
            }
        }

        // Check time limit
        if let Some(max_time) = self.limits.time {
            if self.start_time.elapsed() >= max_time {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::board::Position;

    #[test]
    fn test_search_startpos() {
        let pos = Position::startpos();
        let limits = SearchLimits {
            depth: 3,
            time: Some(Duration::from_secs(1)),
            nodes: None,
        };

        let mut searcher = Searcher::new(limits);
        let result = searcher.search(&pos);

        // Should find a move
        assert!(result.best_move.is_some());

        // Should have searched some nodes
        assert!(result.stats.nodes > 0);

        // Score should be reasonable
        assert!(result.score.abs() < 1000);
    }

    #[test]
    fn test_search_mate_in_one() {
        // TODO: Create a mate-in-one position and verify the search finds it
    }
}
