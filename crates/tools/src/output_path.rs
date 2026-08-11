//! 入力を truncate しないための出力パス検査。

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

fn canonicalize_predicted_path(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Path has no file name: {}", path.display()))?;
    let canonical_parent = parent
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize parent {}", parent.display()))?;
    Ok(canonical_parent.join(file_name))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_relative_output_path_is_supported() -> Result<()> {
        let predicted = canonicalize_predicted_path(Path::new("out.psv"))?;
        assert_eq!(predicted.file_name(), Some(std::ffi::OsStr::new("out.psv")));
        assert_eq!(predicted.parent(), Some(std::env::current_dir()?.canonicalize()?.as_path()));
        Ok(())
    }
}
