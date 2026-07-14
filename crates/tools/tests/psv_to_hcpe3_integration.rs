//! psv_to_hcpe3 の bit 一致 / 決定性 integration テスト
//!
//! `tests/fixtures/psv_to_hcpe3_yaneuraou_sample.psv` を入力に、cshogi 製オラクル
//! （`psv_to_hcpe3.py` / dlshogi `psv_to_hcpe.py`）の出力 `.hcpe3` / `.hcpe` と
//! byte 完全一致することを確認する。旧形式 fixture は混入ガードの回帰テストに使う。

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_psv_to_hcpe3");

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn run(input: &Path, output: &Path, format: &str, threads: &str, chunk: &str) {
    let status = Command::new(BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--format",
            format,
            "--threads",
            threads,
            "--chunk",
            chunk,
        ])
        .status()
        .expect("failed to run psv_to_hcpe3");
    assert!(status.success(), "psv_to_hcpe3 exited with failure");
}

#[test]
fn legacy_move16_fixture_is_rejected() {
    let input = fixture("psv_to_hcpe3_sample.psv");
    let out = std::env::temp_dir().join("psv_to_hcpe3_it_legacy.bin");
    let status = Command::new(BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            out.to_str().unwrap(),
        ])
        .status()
        .expect("failed to run psv_to_hcpe3");
    assert!(!status.success(), "legacy move16 input must be rejected");
}

// 実 YaneuraOu PSV 形式（成り=bit15 / 駒打ち=bit14+from=駒種）の move16 を含む fixture。
// 旧 0x1800 減算方式は bit15 の成りを誤変換するため、この fixture は回帰ガードになる。
#[test]
fn hcpe3_matches_cshogi_oracle_yaneuraou_format() {
    let input = fixture("psv_to_hcpe3_yaneuraou_sample.psv");
    let expected = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.hcpe3")).unwrap();
    let out = std::env::temp_dir().join("psv_to_hcpe3_it_yo_hcpe3.bin");
    run(&input, &out, "hcpe3", "1", "200000");
    assert_eq!(
        std::fs::read(&out).unwrap(),
        expected,
        "real-YaneuraOu PSV (bit15 promote / bit14 drop) must byte-match the cshogi oracle"
    );
}

#[test]
fn hcpe_matches_cshogi_oracle_yaneuraou_format() {
    let input = fixture("psv_to_hcpe3_yaneuraou_sample.psv");
    let expected = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.hcpe")).unwrap();
    let out = std::env::temp_dir().join("psv_to_hcpe3_it_yo_hcpe.bin");
    run(&input, &out, "hcpe", "1", "200000");
    assert_eq!(
        std::fs::read(&out).unwrap(),
        expected,
        "real-YaneuraOu PSV hcpe output must byte-match the cshogi oracle"
    );
}

#[test]
fn trailing_partial_bytes_are_ignored() {
    // 末尾にレコード長未満の半端バイトがあっても、完全なレコードの出力は ground truth と一致し、
    // ツールは成功終了する（半端バイトはスキップ）。
    let expected = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.hcpe3")).unwrap();
    let mut psv = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.psv")).unwrap();
    psv.extend_from_slice(&[0u8; 7]); // 40 バイト境界に満たない末尾
    let truncated_input = std::env::temp_dir().join("psv_to_hcpe3_it_trailing.psv");
    std::fs::write(&truncated_input, &psv).unwrap();
    let out = std::env::temp_dir().join("psv_to_hcpe3_it_trailing.bin");
    run(&truncated_input, &out, "hcpe3", "1", "200000");
    assert_eq!(
        std::fs::read(&out).unwrap(),
        expected,
        "trailing partial bytes must not affect full-record output"
    );
}

#[test]
fn output_path_with_tmp_extension_is_not_truncated() {
    // `--output *.tmp` でも一時ファイル（.partial 付与）と最終パスが衝突せず正しく出力される。
    let input = fixture("psv_to_hcpe3_yaneuraou_sample.psv");
    let expected = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.hcpe3")).unwrap();
    let out = std::env::temp_dir().join("psv_to_hcpe3_it_out.tmp");
    run(&input, &out, "hcpe3", "1", "200000");
    assert_eq!(std::fs::read(&out).unwrap(), expected, "output must be correct for *.tmp path");
}

#[test]
fn limit_restricts_output_record_count() {
    // --limit N は先頭 N レコードだけ変換する（出力 = N × 46 バイト）。
    let input = fixture("psv_to_hcpe3_yaneuraou_sample.psv");
    let out = std::env::temp_dir().join("psv_to_hcpe3_it_limit.bin");
    let status = Command::new(BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            out.to_str().unwrap(),
            "--format",
            "hcpe3",
            "--limit",
            "10",
        ])
        .status()
        .expect("failed to run psv_to_hcpe3");
    assert!(status.success());
    assert_eq!(
        std::fs::metadata(&out).unwrap().len(),
        10 * 46,
        "--limit 10 must emit 10 records"
    );
}

#[test]
fn record_without_move_is_skipped_and_counted() {
    let dir = tempfile::TempDir::new().unwrap();
    let input = dir.path().join("terminal.psv");
    let out = dir.path().join("terminal.hcpe3");
    let fixture_bytes = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.psv")).unwrap();
    let mut terminal = fixture_bytes[..40].to_vec();
    terminal[34..36].copy_from_slice(&0u16.to_le_bytes());
    std::fs::write(&input, terminal).unwrap();

    let output = Command::new(BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            out.to_str().unwrap(),
            "--format",
            "hcpe3",
        ])
        .output()
        .expect("failed to run psv_to_hcpe3");

    assert!(output.status.success());
    assert_eq!(std::fs::metadata(out).unwrap().len(), 0);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("有効な着手を持たないレコードをスキップ: 1 レコード (move16=0)")
    );
}

#[test]
fn output_is_thread_count_independent() {
    // 出力はスレッド数・チャンク境界に依らず bit 一致でなければならない。
    let input = fixture("psv_to_hcpe3_yaneuraou_sample.psv");
    let out1 = std::env::temp_dir().join("psv_to_hcpe3_it_t1.bin");
    let out4 = std::env::temp_dir().join("psv_to_hcpe3_it_t4.bin");
    run(&input, &out1, "hcpe3", "1", "200000");
    run(&input, &out4, "hcpe3", "4", "7");
    assert_eq!(
        std::fs::read(&out1).unwrap(),
        std::fs::read(&out4).unwrap(),
        "output must be identical regardless of thread count and chunk size"
    );
}
