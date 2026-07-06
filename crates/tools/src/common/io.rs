//! ファイルI/Oユーティリティ（gzip対応）

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;

const READER_BUF_CAP: usize = 128 * 1024; // 128 KiB

pub fn open_reader<P: AsRef<Path>>(path: P) -> io::Result<Box<dyn BufRead>> {
    let p = path.as_ref();
    if p.to_string_lossy() == "-" {
        return Ok(Box::new(BufReader::with_capacity(READER_BUF_CAP, io::stdin())));
    }
    let f = File::open(p)?;
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();

    if ext == "gz" {
        let dec = flate2::read::GzDecoder::new(f);
        return Ok(Box::new(BufReader::with_capacity(READER_BUF_CAP, dec)));
    }
    Ok(Box::new(BufReader::with_capacity(READER_BUF_CAP, f)))
}

/// Writer wrapper to propagate finish/close errors for compressed outputs.
#[must_use = "call .close() to propagate compression/IO errors"]
pub enum Writer {
    Plain(BufWriter<File>),
    Stdout(std::io::Stdout),
    Gz(flate2::write::GzEncoder<File>),
}

impl Write for Writer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Writer::Plain(f) => f.write(buf),
            Writer::Stdout(s) => s.write(buf),
            Writer::Gz(e) => e.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Writer::Plain(f) => f.flush(),
            Writer::Stdout(s) => s.flush(),
            Writer::Gz(e) => e.flush(),
        }
    }
}

impl Writer {
    /// Finalize the stream and flush underlying file/stdout.
    pub fn close(self) -> io::Result<()> {
        match self {
            Writer::Plain(f) => {
                let mut file = f.into_inner().map_err(|e| e.into_error())?;
                file.flush()
            }
            Writer::Stdout(mut s) => s.flush(),
            Writer::Gz(e) => {
                let mut f = e.finish()?;
                f.flush()
            }
        }
    }
}

pub fn open_writer<P: AsRef<Path>>(path: P) -> io::Result<Writer> {
    let p = path.as_ref();
    if p.to_string_lossy() == "-" {
        return Ok(Writer::Stdout(std::io::stdout()));
    }
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    if ext == "gz" {
        let f = File::create(p)?;
        let enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        return Ok(Writer::Gz(enc));
    }
    let f = File::create(p)?;
    Ok(Writer::Plain(BufWriter::new(f)))
}

/// 同一 dir の一時ファイルに書いてから rename で置き換える(部分書き込みで既存
/// ファイルを壊さず、読者は常に完全な内容だけを見る)。親ディレクトリが無ければ作る。
pub fn write_atomic(path: &Path, content: &str) -> anyhow::Result<()> {
    use anyhow::Context;
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent).with_context(|| format!("create dir {}", parent.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temp file in {}", parent.display()))?;
    tmp.write_all(content.as_bytes())
        .with_context(|| format!("write temp file for {}", path.display()))?;
    tmp.persist(path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}
