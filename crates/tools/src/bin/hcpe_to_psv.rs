//! hcpe（cshogi HuffmanCodedPosAndEval, 38B/レコード）→ PSV（PackedSfenValue, 40B/レコード）
//! 変換ツール。
//!
//! 外部公開の hcpe 教師/検証プール（例: dlshogi 系の floodgate 検証局面）を、
//! nnue-train の `--data` / `--test-data` が読む PSV 形式へ変換する。
//!
//! # フィールド対応
//!
//! - 局面: `HuffmanCodedPos`（Apery/cshogi 形式）→ `PackedSfen`（YaneuraOu 形式）。
//!   Huffman テーブルが異なるため `Position` 経由で再パックする。
//! - eval: 手番側視点 cp（両形式で同一規約）をそのままコピー。
//! - bestMove16: cshogi 形式 → YaneuraOu Move16 形式（駒打ちの駒種 index が 1 ずれる）。
//! - gameResult: 絶対視点（0=draw / 1=black_win / 2=white_win）→ 手番側視点
//!   （1=win / -1=loss / 0=draw）。
//! - game_ply: hcpe には手数が無いため 1 固定（`unpack_hcp` の SFEN 手数をそのまま使う）。
//!
//! # 使用例
//!
//! ```bash
//! cargo run --release -p tools --bin hcpe_to_psv -- \
//!   --input "$SHOGI_DATA/validation/floodgate_hcpe_yamaoka/floodgate.hcpe" \
//!   --output "$SHOGI_DATA/validation/floodgate_hcpe_yamaoka/floodgate.psv"
//! ```
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;

use clap::Parser;
use rshogi_core::position::Position;
use rshogi_core::types::Color;
use tools::packed_sfen::{
    PackedSfenValue, hcpe_move16_to_move, move_to_move16, pack_position, unpack_hcp,
};
use tools::teacher_labeler::HCPE_RECORD_SIZE;

#[derive(Parser, Debug)]
#[command(
    name = "hcpe_to_psv",
    about = "hcpe (38B/レコード) を PSV (PackedSfenValue 40B/レコード) に変換する"
)]
struct Args {
    /// 入力 hcpe ファイル（カンマ区切りで複数可）。--input-dir と排他
    #[arg(long)]
    input: Option<String>,

    /// 入力ディレクトリ。--pattern と組み合わせて使用。--input と排他
    #[arg(long)]
    input_dir: Option<PathBuf>,

    /// --input-dir 使用時の glob パターン
    #[arg(long, default_value = "*.hcpe")]
    pattern: String,

    /// 出力ファイルパス（PSV 形式）。入力順を保持して連結する
    #[arg(long)]
    output: PathBuf,
}

#[derive(Default)]
struct Stats {
    converted: u64,
    decode_errors: u64,
    move_errors: u64,
    result_errors: u64,
}

/// gameResult（絶対視点 0=draw / 1=black_win / 2=white_win）を手番側視点の
/// PSV game_result（1=win / -1=loss / 0=draw）へ変換する。0/1/2 以外は `None`。
fn convert_game_result(game_result: u8, stm: Color) -> Option<i8> {
    match game_result {
        0 => Some(0),
        1 => Some(if stm == Color::Black { 1 } else { -1 }),
        2 => Some(if stm == Color::White { 1 } else { -1 }),
        _ => None,
    }
}

/// hcpe 1 レコードを PSV 1 レコードへ変換する。壊れたレコードは `Err` で返し、
/// 呼び出し側で件数を数えて skip する。
fn convert_record(bytes: &[u8; HCPE_RECORD_SIZE], stats: &mut Stats) -> Option<PackedSfenValue> {
    let mut hcp = [0u8; 32];
    hcp.copy_from_slice(&bytes[0..32]);
    let eval = i16::from_le_bytes([bytes[32], bytes[33]]);
    let best_move16 = u16::from_le_bytes([bytes[34], bytes[35]]);
    let game_result = bytes[36];

    let sfen = match unpack_hcp(&hcp) {
        Ok(s) => s,
        Err(_) => {
            stats.decode_errors += 1;
            return None;
        }
    };
    let mut pos = Position::new();
    if pos.set_sfen(&sfen).is_err() {
        stats.decode_errors += 1;
        return None;
    }

    let mv = hcpe_move16_to_move(best_move16);
    let move16 = move_to_move16(mv);
    if move16 == 0 {
        stats.move_errors += 1;
        return None;
    }

    let Some(psv_result) = convert_game_result(game_result, pos.side_to_move()) else {
        stats.result_errors += 1;
        return None;
    };

    stats.converted += 1;
    Some(PackedSfenValue {
        sfen: pack_position(&pos),
        score: eval,
        move16,
        game_ply: pos.game_ply() as u16,
        game_result: psv_result,
        padding: 0,
    })
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    let paths = tools::common::dedup::collect_input_paths(
        args.input.as_deref(),
        args.input_dir.as_ref(),
        &args.pattern,
    )?;
    if paths.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "入力ファイルが見つかりません"));
    }
    for p in &paths {
        let len = std::fs::metadata(p)?.len();
        if len % HCPE_RECORD_SIZE as u64 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} のサイズ {len} が hcpe レコード長 {HCPE_RECORD_SIZE} の倍数ではありません",
                    p.display()
                ),
            ));
        }
    }

    let out_file = File::create(&args.output)?;
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, out_file);
    let mut stats = Stats::default();

    for path in &paths {
        eprintln!("Reading: {}", path.display());
        let mut reader = BufReader::with_capacity(8 * 1024 * 1024, File::open(path)?);
        let mut buf = [0u8; HCPE_RECORD_SIZE];
        loop {
            match reader.read_exact(&mut buf) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
            if let Some(psv) = convert_record(&buf, &mut stats) {
                writer.write_all(&psv.to_bytes())?;
            }
        }
    }
    writer.flush()?;

    println!("=== hcpe → PSV Summary ===");
    println!("Input files:     {}", paths.len());
    println!("Converted:       {}", stats.converted);
    println!("Decode errors:   {}", stats.decode_errors);
    println!("Move errors:     {}", stats.move_errors);
    println!("Result errors:   {}", stats.result_errors);
    println!("Output file:     {}", args.output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tools::packed_sfen::{move_to_hcpe_move16, pack_position_hcp, unpack_sfen};

    fn make_hcpe_record(
        pos: &Position,
        eval: i16,
        best_move16: u16,
        game_result: u8,
    ) -> [u8; HCPE_RECORD_SIZE] {
        let mut rec = [0u8; HCPE_RECORD_SIZE];
        rec[0..32].copy_from_slice(&pack_position_hcp(pos));
        rec[32..34].copy_from_slice(&eval.to_le_bytes());
        rec[34..36].copy_from_slice(&best_move16.to_le_bytes());
        rec[36] = game_result;
        rec
    }

    #[test]
    fn convert_record_roundtrips_position_eval_move_result() {
        let mut pos = Position::new();
        pos.set_hirate();
        // 平手初期局面の ▲7六歩 (77→76)
        let mv = {
            use rshogi_core::movegen::{MoveList, generate_legal_all};
            let mut moves = MoveList::new();
            generate_legal_all(&pos, &mut moves);
            *moves.iter().find(|m| !m.is_drop() && m.to_usi() == "7g7f").unwrap()
        };

        let rec = make_hcpe_record(&pos, -123, move_to_hcpe_move16(mv), 2);
        let mut stats = Stats::default();
        let psv = convert_record(&rec, &mut stats).unwrap();

        assert_eq!(unpack_sfen(&psv.sfen).unwrap(), unpack_sfen(&pack_position(&pos)).unwrap());
        assert_eq!(psv.score, -123);
        assert_eq!(psv.move16, move_to_move16(mv));
        // white_win で手番=先手 → 手番側視点 loss
        assert_eq!(psv.game_result, -1);
        assert_eq!(stats.converted, 1);
    }

    #[test]
    fn convert_game_result_maps_absolute_to_stm_view() {
        assert_eq!(convert_game_result(0, Color::Black), Some(0));
        assert_eq!(convert_game_result(1, Color::Black), Some(1));
        assert_eq!(convert_game_result(1, Color::White), Some(-1));
        assert_eq!(convert_game_result(2, Color::White), Some(1));
        assert_eq!(convert_game_result(3, Color::Black), None);
    }

    #[test]
    fn convert_record_rejects_broken_records() {
        let mut stats = Stats::default();
        // 全ゼロ: bestMove16=0 → move error (hcp 全ゼロは decode 失敗が先でも可)
        let rec = [0u8; HCPE_RECORD_SIZE];
        assert!(convert_record(&rec, &mut stats).is_none());
        assert_eq!(stats.converted, 0);
    }
}
