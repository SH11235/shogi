//! 定跡（opening book）の USI 統合テスト。
//!
//! - YANEURAOU-DB2016 形式の実 `.db` フィクスチャを与えて position→go で bestmove が返ること
//! - BookFile=no_book（既定）では定跡関連の出力が一切無く従来挙動が不変であること
//! - 片側正規化定跡に FlippedBook でヒットすること

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

/// Material 評価で動作させる共通初期化（NNUE ファイル不要）。
const MATERIAL_INIT: &str = "usi\nsetoption name MaterialLevel value 9\n";

/// テスト用の一意な一時ファイルパスを作り、内容を書き込んで返す。
fn write_temp_db(tag: &str, contents: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let unique = format!(
        "rshogi_book_test_{}_{}_{}.db",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    path.push(unique);
    std::fs::write(&path, contents).expect("write temp .db fixture");
    path
}

/// エンジンに一連のコマンドを流し込み stdout をまとめて返す。
fn run_engine(input: &str) -> String {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"));
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn engine");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(input.as_bytes()).expect("write");
    }

    let output = child.wait_with_output().expect("wait output");
    assert!(output.status.success(), "engine exited with failure");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `bestmove` 行（最初のもの）を取り出す。
fn first_bestmove(stdout: &str) -> Option<&str> {
    stdout.lines().find(|l| l.starts_with("bestmove"))
}

#[test]
fn book_hit_returns_bestmove_from_db() {
    // YANEURAOU-DB2016 形式の実 .db。平手初期局面に 7g7f(ponder 3c3d) を 1 手だけ登録。
    let db = "#YANEURAOU-DB2016 1.00\n\
              sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\n\
              7g7f 3c3d 30 16 100\n";
    let path = write_temp_db("hit", db);
    let path_str = path.to_string_lossy();

    let input = format!(
        "{MATERIAL_INIT}\
         setoption name BookFile value {path_str}\n\
         isready\n\
         position startpos\n\
         go depth 10\n\
         quit\n"
    );
    let stdout = run_engine(&input);
    let _ = std::fs::remove_file(&path);

    assert!(stdout.contains("book loaded"), "book load info missing:\n{stdout}");
    // 候補が 1 手なので抽選に依らず 7g7f ponder 3c3d が確定する。
    assert_eq!(
        first_bestmove(&stdout),
        Some("bestmove 7g7f ponder 3c3d"),
        "unexpected bestmove:\n{stdout}"
    );
}

#[test]
fn flipped_book_hit_on_one_sided_db() {
    // 片側正規化定跡: 後手番局面のみを flip した先手番の正準局面を登録。
    // 後手番局面 (2 手目、白番) を probe すると flip でヒットする。
    // flipped_key("...w - 2") = 先手番の対称局面。その黒視点の手 7g7f を登録すると、
    // 後手番局面では 3c3d に反転される。
    let flipped_sfen = "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 2";
    let db = format!(
        "#YANEURAOU-DB2016 1.00\n\
         sfen {flipped_sfen}\n\
         7g7f 8c8d 20 16 50\n"
    );
    let path = write_temp_db("flip", &db);
    let path_str = path.to_string_lossy();

    // 後手番局面 (先手が 7g7f を指した直後) を position で与える。
    let input = format!(
        "{MATERIAL_INIT}\
         setoption name BookFile value {path_str}\n\
         isready\n\
         position startpos moves 7g7f\n\
         go depth 10\n\
         quit\n"
    );
    let stdout = run_engine(&input);
    let _ = std::fs::remove_file(&path);

    // flip で戻された白の 3c3d(ponder 2g2f)が返る。
    assert_eq!(
        first_bestmove(&stdout),
        Some("bestmove 3c3d ponder 2g2f"),
        "flipped book hit failed:\n{stdout}"
    );
}

#[test]
fn no_book_default_is_unchanged() {
    // BookFile 未設定（既定 no_book）では定跡は一切使われず、通常探索の bestmove が返る。
    let input = format!(
        "{MATERIAL_INIT}\
         isready\n\
         position startpos\n\
         go depth 6\n\
         quit\n"
    );
    let stdout = run_engine(&input);

    // 定跡関連の出力が一切無いこと（従来挙動が完全不変）。
    assert!(!stdout.contains("book loaded"), "unexpected book output:\n{stdout}");
    // 通常探索の bestmove が返ること。
    let bm = first_bestmove(&stdout).expect("bestmove present");
    assert!(bm.starts_with("bestmove"), "no bestmove:\n{stdout}");
    // 探索由来なので info depth 行が出ているはず（book hit ならスキップされる）。
    assert!(stdout.contains("info "), "expected search info lines:\n{stdout}");
}

#[test]
fn own_book_false_falls_back_to_search() {
    // 定跡はロードするが USI_OwnBook=false なら probe をスキップして通常探索する。
    let db = "#YANEURAOU-DB2016 1.00\n\
              sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\n\
              7g7f 3c3d 30 16 100\n";
    let path = write_temp_db("ownbook", db);
    let path_str = path.to_string_lossy();

    let input = format!(
        "{MATERIAL_INIT}\
         setoption name BookFile value {path_str}\n\
         setoption name USI_OwnBook value false\n\
         isready\n\
         position startpos\n\
         go depth 6\n\
         quit\n"
    );
    let stdout = run_engine(&input);
    let _ = std::fs::remove_file(&path);

    // book はロードされるが probe されないので、通常探索の info 行が出る。
    assert!(stdout.contains("book loaded"), "book should still load:\n{stdout}");
    assert!(stdout.contains("info "), "expected search info lines:\n{stdout}");
    assert!(first_bestmove(&stdout).is_some(), "bestmove present:\n{stdout}");
}
