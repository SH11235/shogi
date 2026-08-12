//! 入力を truncate しないための出力パス検査。

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

fn canonicalize_predicted_path(path: &Path) -> Result<PathBuf> {
    crate::common::dedup::canonicalize_maybe_new(path)
        .with_context(|| format!("Failed to canonicalize {}", path.display()))
}

fn is_same_file(a: &Path, b: &Path) -> Result<bool> {
    match same_file::is_same_file(a, b) {
        Ok(same) => Ok(same),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| {
            format!("Failed to compare file identities: {} and {}", a.display(), b.display())
        }),
    }
}

/// 出力が入力実体を指す場合は、truncate 前に拒否する。
pub fn ensure_safe_output_path(output: &Path, input: &Path) -> Result<()> {
    let canonical_input = input
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize input {}", input.display()))?;
    if let Ok(meta) = fs::symlink_metadata(output)
        && meta.file_type().is_symlink()
    {
        anyhow::bail!("Output path is a symlink: {}", output.display());
    }
    let predicted = canonicalize_predicted_path(output)?;
    if predicted == canonical_input || is_same_file(output, &canonical_input)? {
        anyhow::bail!(
            "Output path resolves to input file: {} -> {}",
            output.display(),
            canonical_input.display()
        );
    }
    Ok(())
}

/// 二つの出力パスが同じ実体または同じ生成予定パスなら拒否する。
pub fn ensure_distinct_output_paths(a: &Path, b: &Path) -> Result<()> {
    let predicted_a = canonicalize_predicted_path(a)?;
    let predicted_b = canonicalize_predicted_path(b)?;
    if predicted_a == predicted_b || is_same_file(a, b)? {
        anyhow::bail!("Output paths resolve to the same file: {} and {}", a.display(), b.display());
    }
    Ok(())
}

/// 作成済みファイル群の実体をペア毎に比較し、同一実体があれば拒否する。
/// 未作成パスの予測比較は case-insensitive filesystem の alias
/// (`Score.bin` と `score.bin` 等) を検出できないため、staging ファイルを
/// 作成した直後・書き込み前にこの実体検査を通すこと。
pub fn ensure_created_paths_distinct(paths: &[&Path]) -> Result<()> {
    for i in 0..paths.len() {
        for j in i + 1..paths.len() {
            ensure_not_same_entity(paths[i], paths[j])?;
        }
    }
    Ok(())
}

/// 作成済みパス群と対象パス群の実体をクロス比較し、同一実体があれば拒否する。
/// staging の作成が別出力の最終パスを実体化させる alias
/// (例: `out_scores=X` の staging `X.partial` が最終出力 `x.partial` と一致)
/// は staging 同士の比較では検出できないため、staging×最終出力にも掛けること。
/// 対象パスが未作成 (NotFound) のペアは不一致として扱う。
pub fn ensure_no_entity_overlap(created: &[&Path], targets: &[&Path]) -> Result<()> {
    for created_path in created {
        for target in targets {
            ensure_not_same_entity(created_path, target)?;
        }
    }
    Ok(())
}

fn ensure_not_same_entity(a: &Path, b: &Path) -> Result<()> {
    let same = match same_file::is_same_file(a, b) {
        Ok(same) => same,
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error).with_context(|| {
                format!("Failed to compare file identities: {} and {}", a.display(), b.display())
            });
        }
    };
    anyhow::ensure!(
        !same,
        "Output paths resolve to the same file: {} and {}",
        a.display(),
        b.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_overlap_detects_hardlinked_paths_and_tolerates_missing() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let created = dir.path().join("created.partial");
        let target = dir.path().join("final.out");
        std::fs::write(&created, [0u8])?;
        std::fs::hard_link(&created, &target)?;
        assert!(ensure_no_entity_overlap(&[&created], &[&target]).is_err());
        assert!(ensure_no_entity_overlap(&[&created], &[&dir.path().join("missing")]).is_ok());
        Ok(())
    }

    #[test]
    fn bare_relative_output_path_is_supported() -> Result<()> {
        let predicted = canonicalize_predicted_path(Path::new("out.psv"))?;
        assert_eq!(predicted.file_name(), Some(std::ffi::OsStr::new("out.psv")));
        assert_eq!(predicted.parent(), Some(std::env::current_dir()?.canonicalize()?.as_path()));
        Ok(())
    }
}
