//! E4 active index の cross-repo golden dumper (rshogi 側)。
//!
//! 共有 SFEN 群について各 (perspective, config) の E4 active index を sorted 出力する。
//! tatara 側 dumper と同一 SFEN・同一形式で出力し、diff が空になることが Golden Forward
//! (index bit 一致) ゲート。形式: `<sfen_no> <B|W> <config_name> : <idx> <idx> ...`

use rshogi_core::nnue::{E4Config, e4_active_indices_for_sfen};
use rshogi_core::types::Color;

fn main() {
    // 玉隣接・成駒・slider 遮蔽・near-king を含む固定局面 (順序固定、tatara 側と一致必須)。
    let sfens = [
        "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        "l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1",
        "6n1l/2+S1k4/2lp4p/1np1B2b1/3PP4/1N1S3rP/1P2+pPP+p1/1p1G5/3KG2r1 b GSN2L4Pgs2p 1",
        "l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
        "lnsgkgsnl/1r5b1/ppppppppp/9/4P4/9/PPPP1PPPP/1B5R1/LNSGKGSNL b - 1",
    ];
    let configs = [
        ("e4_2x2_kingfixed", E4Config::E4_2X2_KINGFIXED),
        ("e4_2x2_kingbucketed", E4Config::E4_2X2_KINGBUCKETED),
        ("kpe9_kingfixed", E4Config::KPE9_KINGFIXED),
        ("kpe9_kingbucketed", E4Config::KPE9_KINGBUCKETED),
    ];

    for (no, sfen) in sfens.iter().enumerate() {
        for persp in [Color::Black, Color::White] {
            let tag = if persp == Color::Black { "B" } else { "W" };
            for (name, cfg) in configs.iter() {
                let idx = e4_active_indices_for_sfen(sfen, *cfg, persp)
                    .unwrap_or_else(|e| panic!("sfen {no} decode failed: {e:?}"));
                let joined = idx.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" ");
                println!("{no} {tag} {name} : {joined}");
            }
        }
    }
}
