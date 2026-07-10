use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use rshogi_core::position::Position;
use tempfile::TempDir;
use tools::packed_sfen::{PackedSfenValue, pack_position};

const BIN: &str = env!("CARGO_BIN_EXE_relabel_psv");

fn record(pos: &Position, score: i16, game_ply: u16, game_result: i8) -> PackedSfenValue {
    PackedSfenValue {
        sfen: pack_position(pos),
        score,
        move16: 0,
        game_ply,
        game_result,
        padding: 0,
    }
}

fn write_psv(path: &Path, records: &[PackedSfenValue]) {
    let mut file = File::create(path).unwrap();
    for record in records {
        file.write_all(&record.to_bytes()).unwrap();
    }
}

fn read_psv(path: &Path) -> Vec<PackedSfenValue> {
    std::fs::read(path)
        .unwrap()
        .chunks_exact(PackedSfenValue::SIZE)
        .map(|bytes| PackedSfenValue::from_bytes(bytes).unwrap())
        .collect()
}

fn hirate() -> Position {
    let mut pos = Position::new();
    pos.set_hirate();
    pos
}

#[test]
fn score_is_replaced_from_side_to_move_game_result_and_is_deterministic() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output_a = dir.path().join("output-a.psv");
    let output_b = dir.path().join("output-b.psv");
    let pos = hirate();
    write_psv(
        &input,
        &[
            record(&pos, -12, 1, 1),
            record(&pos, 34, 2, -1),
            record(&pos, 56, 3, 0),
        ],
    );

    for output in [&output_a, &output_b] {
        let result = Command::new(BIN)
            .args([
                "--input",
                input.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
                "--win-cp",
                "1234",
            ])
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stderr.contains("input=3 win=1 loss=1 draw=1"));
    }

    let scores: Vec<i16> = read_psv(&output_a).iter().map(|record| record.score).collect();
    assert_eq!(scores, [1234, -1234, 0]);
    assert_eq!(std::fs::read(output_a).unwrap(), std::fs::read(output_b).unwrap());
}

#[test]
fn declaration_override_uses_side_to_move_declaration_win() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let mut pos = Position::new();
    pos.set_sfen("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1")
        .unwrap();
    write_psv(&input, &[record(&pos, -100, 1, -1)]);

    let result = Command::new(BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--win-cp",
            "2222",
            "--declaration-override",
        ])
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(read_psv(&output)[0].score, 2222);
    assert!(String::from_utf8_lossy(&result.stderr).contains("declaration_override=1"));
}

#[test]
fn deblunder_modes_drop_before_the_selected_diversion_boundary() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let sidecar = dir.path().join("game_ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    let records: Vec<_> = (2..=6).map(|ply| record(&pos, 10, ply, 1)).collect();
    write_psv(&input, &records);
    let mut ids = File::create(&sidecar).unwrap();
    for _ in &records {
        ids.write_all(&42u32.to_le_bytes()).unwrap();
    }
    std::fs::write(
        &diversions,
        concat!(
            "{\"type\":\"meta\"}\n",
            "{\"type\":\"result\",\"game_id\":42,",
            "\"start_sfen\":\"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\",",
            "\"diversions\":[{\"ply\":5},{\"ply\":3}]}\n"
        ),
    )
    .unwrap();

    for (mode, expected_plies, expected_drops) in [
        ("drop-before-last", vec![6], 4),
        ("drop-before-any", vec![4, 5, 6], 2),
    ] {
        let output = dir.path().join(format!("{mode}.psv"));
        let result = Command::new(BIN)
            .args([
                "--input",
                input.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
                "--deblunder",
                "--game-id-sidecar",
                sidecar.to_str().unwrap(),
                "--diversions",
                diversions.to_str().unwrap(),
                "--deblunder-mode",
                mode,
            ])
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let plies: Vec<u16> = read_psv(&output).iter().map(|record| record.game_ply).collect();
        assert_eq!(plies, expected_plies);
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains(&format!("deblunder_drop={expected_drops}"))
        );
    }
}

#[test]
fn deblunder_converts_relative_diversion_ply_from_midgame_start_sfen() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let sidecar = dir.path().join("game_ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    let records: Vec<_> = (100..=103).map(|ply| record(&pos, 10, ply, 1)).collect();
    write_psv(&input, &records);
    let mut ids = File::create(&sidecar).unwrap();
    for _ in &records {
        ids.write_all(&7u32.to_le_bytes()).unwrap();
    }
    std::fs::write(
        &diversions,
        concat!(
            "{\"type\":\"result\",\"game_id\":7,",
            "\"start_sfen\":\"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 100\",",
            "\"diversions\":[{\"ply\":2}]}\n"
        ),
    )
    .unwrap();

    let result = Command::new(BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--deblunder",
            "--game-id-sidecar",
            sidecar.to_str().unwrap(),
            "--diversions",
            diversions.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let plies: Vec<u16> = read_psv(&output).iter().map(|record| record.game_ply).collect();
    assert_eq!(plies, [102, 103]);
    assert!(String::from_utf8_lossy(&result.stderr).contains("deblunder_drop=2"));
}
