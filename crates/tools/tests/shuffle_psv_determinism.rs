use std::fs;
use std::process::Command;

const RECORD_SIZE: usize = 40;

fn run_shuffle(input: &std::path::Path, output: &std::path::Path, threads: usize) -> String {
    let result = Command::new(env!("CARGO_BIN_EXE_shuffle_psv"))
        .args([
            "--input",
            input.to_str().expect("UTF-8 input path"),
            "--output",
            output.to_str().expect("UTF-8 output path"),
            "--seed",
            "20260801",
            "--chunk-size",
            "500",
        ])
        .env("RAYON_NUM_THREADS", threads.to_string())
        .env("RUST_LOG", "info")
        .output()
        .expect("shuffle_psv を実行できること");

    assert!(
        result.status.success(),
        "shuffle_psv failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stderr).expect("ログが UTF-8 であること")
}

#[test]
fn chunked_output_is_independent_of_threads_and_batch_size() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作成できること");
    let input = dir.path().join("input.psv");
    let single_output = dir.path().join("single.psv");
    let multi_output = dir.path().join("multi.psv");

    let mut records = vec![0u8; 3_000 * RECORD_SIZE];
    for (index, record) in records.chunks_exact_mut(RECORD_SIZE).enumerate() {
        record[..8].copy_from_slice(&(index as u64).to_le_bytes());
        for (offset, byte) in record[8..].iter_mut().enumerate() {
            *byte = (index.wrapping_mul(31).wrapping_add(offset) & 0xff) as u8;
        }
    }
    fs::write(&input, records).expect("入力 PSV を作成できること");

    let single_log = run_shuffle(&input, &single_output, 1);
    let multi_log = run_shuffle(&input, &multi_output, 4);

    assert!(single_log.contains("Pass 2 batching (threads: 1,"));
    assert!(multi_log.contains("Pass 2 batching (threads: 4,"));
    assert_eq!(
        fs::read(single_output).expect("単一スレッド出力を読めること"),
        fs::read(multi_output).expect("複数スレッド出力を読めること"),
    );
}
