//! ONNX Runtime (ort) をロードしたプロセスの終了処理。
//!
//! TensorRT EP をロードしたプロセスは、通常の exit(3) が走らせる atexit /
//! `_dl_fini` のデストラクタ連鎖 (onnxruntime provider の unload と libcuda の
//! 後始末) が glibc ヒープを破壊し、全処理の正常完了後に
//! `corrupted double-linked list` abort (exit code 134) を間欠的に起こす。
//! AddressSanitizer 下では再現しない (プロセス全体が単一アロケータに統一される)
//! ことからライブラリ間の解放不整合であり、このリポジトリ側のコードでは
//! 修正できない。ORT セッションを一切作らず TensorRT EP の登録に失敗しただけの
//! プロセスでも発生する。

/// 全出力を flush した上で、C/C++ デストラクタと atexit を踏まずに即時終了する。
///
/// ONNX 推論を使った処理の正常終了経路でのみ呼ぶこと。ファイル出力は呼び出し
/// 前に呼び出し側で flush / drop されている必要がある (Rust のデストラクタも
/// 走らないため、生存中の `BufWriter` の暗黙 flush には頼れない)。
pub fn exit_skipping_ort_teardown(code: i32) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    #[cfg(unix)]
    // SAFETY: `_exit(2)` は async-signal-safe なプロセス即時終了で、メモリ安全性の
    // 不変条件を破らない。デストラクタ skip で取りこぼしが出ないことは上記の
    // 呼び出し契約 (出力 flush 済み) が担保する。
    unsafe {
        libc::_exit(code)
    }
    #[cfg(not(unix))]
    std::process::exit(code)
}
