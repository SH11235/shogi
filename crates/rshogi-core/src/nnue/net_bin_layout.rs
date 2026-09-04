//! LayerStacks `.bin` の feature 非依存レイアウト走査。

use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::ops::Range;

use super::bona_piece_effect_bucket::EffectBucketConfig;
use super::constants::{
    DEFAULT_NUM_BUCKETS, HALFKA_DIMENSIONS, HALFKA_HM_DIMENSIONS, HALFKA_HM_SPLIT_DIMENSIONS,
    HALFKA_MERGED_DIMENSIONS, HALFKP_DIMENSIONS, MAX_ARCH_LEN, MAX_LAYER_STACK_BUCKETS,
    NNUE_VERSION_HALFKA, NNUE_VERSION_LAYERSTACK_NUM_BUCKETS,
};
use super::leb128::{LEB128_MAGIC, MAX_COMPRESSED_SIZE};
use super::net_delta::{NetCoefficientId, NetDeltaError, NetTensorKind, NetTensorShape};
use super::spec::{
    FeatureSet, parse_arch_dimensions, parse_feature_input_dimensions,
    parse_layer_stacks_feature_set_keyword, validate_layer_stacks_architecture_header,
};

/// tensor の biases / weights byte 範囲。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorBinLayout {
    /// biases の byte 範囲。
    pub biases: Range<usize>,
    /// weights の byte 範囲。
    pub weights: Range<usize>,
}

/// Feature Transformer の格納方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtBinEncoding {
    /// biases と weights を連結した旧 bullet 形式の LEB128 1 ブロック。
    Leb128Combined,
    /// biases と weights を分けた YO 形式の LEB128 2 ブロック。
    Leb128Split,
}

/// Feature Transformer ブロックのレイアウト。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureTransformerBinLayout {
    /// FT hash の byte 範囲。
    pub hash: Range<usize>,
    /// FT の格納方式。
    pub encoding: FtBinEncoding,
    /// bias 値を格納する byte 範囲。LEB128 では block header を除く。
    pub biases: Range<usize>,
    /// weight 値を格納する byte 範囲。LEB128 では block header を除く。
    pub weights: Range<usize>,
}

/// 1 bucket 分の全結合層レイアウト。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerStackBucketBinLayout {
    /// bucket 固有 FC hash の byte 範囲。
    pub fc_hash: Range<usize>,
    /// 第 1 FC 層。
    pub l1: TensorBinLayout,
    /// 第 2 FC 層。
    pub l2: TensorBinLayout,
    /// output 層。
    pub output: TensorBinLayout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredBinBlock {
    source: Range<usize>,
    bytes: Vec<u8>,
}

/// LayerStacks `.bin` 全体の byte レイアウト。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerStacksBinLayout {
    /// NNUE format version。
    pub version: u32,
    /// network hash。
    pub network_hash: u32,
    /// header の architecture 文字列。
    pub architecture: String,
    /// 格納 bucket 数。
    pub num_buckets: usize,
    /// Feature Transformer 出力次元。
    pub l1: usize,
    /// 第 1 FC 層出力次元。
    pub l2: usize,
    /// 第 2 FC 層出力次元。
    pub l3: usize,
    /// Threat を除く Feature Transformer 入力次元。
    pub ft_input_dimensions: usize,
    /// Feature Transformer ブロック。
    pub feature_transformer: FeatureTransformerBinLayout,
    /// PSQT ブロック。architecture が PSQT を持たなければ `None`。
    pub psqt: Option<TensorBinLayout>,
    /// Threat profile id の byte 範囲。
    pub threat_profile: Option<Range<usize>>,
    /// Threat weights の byte 範囲。
    pub threat_weights: Option<Range<usize>>,
    /// bucket ごとの FC ブロック。
    pub buckets: Vec<LayerStackBucketBinLayout>,
    // FT weights は EffectBucket で GiB 級になるため保持せず、係数列挙に必要な
    // FT bias と小さい FC block だけを保持して常駐量を net の FT サイズから切り離す。
    ft_biases: Vec<i16>,
    fc_data: StoredBinBlock,
}

impl LayerStacksBinLayout {
    /// byte slice を走査し、LayerStacks の各 byte 範囲を返す。
    ///
    /// 実装は [`Self::from_reader`] に委譲する。
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        Self::from_reader(&mut Cursor::new(bytes))
    }

    /// seek 可能な `.bin` を走査し、LayerStacks の各 byte 範囲を返す。
    ///
    /// FT の圧縮 weights payload は読み込まず seek で飛ばす。常駐メモリは decode 済み
    /// FT bias と全 bucket の FC block に限られ、FT weights のサイズには依存しない。
    pub fn from_reader<R: Read + Seek>(reader: &mut R) -> io::Result<Self> {
        let mut cursor = StreamCursor::new(reader)?;
        let version = cursor.read_u32("version")?;
        if version != NNUE_VERSION_HALFKA && version != NNUE_VERSION_LAYERSTACK_NUM_BUCKETS {
            return Err(invalid(format!("unsupported LayerStacks version {version:#x}")));
        }
        let network_hash = cursor.read_u32("network hash")?;
        let arch_len = cursor.read_u32("architecture length")? as usize;
        if arch_len == 0 || arch_len > MAX_ARCH_LEN {
            return Err(invalid(format!("invalid architecture string length: {arch_len}")));
        }
        let architecture_bytes = cursor.read_bytes(arch_len, "architecture string")?;
        let architecture = std::str::from_utf8(&architecture_bytes)
            .map_err(|_| invalid("architecture string is not UTF-8"))?
            .to_owned();
        if !is_layer_stacks_architecture(&architecture) {
            return Err(invalid(format!("architecture is not LayerStacks: {architecture}")));
        }

        let num_buckets = if version == NNUE_VERSION_LAYERSTACK_NUM_BUCKETS {
            cursor.read_u32("num_buckets")? as usize
        } else {
            DEFAULT_NUM_BUCKETS
        };
        if !(1..=MAX_LAYER_STACK_BUCKETS).contains(&num_buckets) {
            return Err(invalid(format!(
                "invalid LayerStacks num_buckets={num_buckets}; expected 1..={MAX_LAYER_STACK_BUCKETS}"
            )));
        }

        let (l1, l2, l3) = parse_arch_dimensions(&architecture);
        if l1 == 0 || !l1.is_multiple_of(2) || l2 < 2 || l3 == 0 {
            return Err(invalid("invalid LayerStacks dimensions"));
        }
        let threat_dimensions = validate_layer_stacks_architecture_header(&architecture)
            .map_err(invalid)?
            .unwrap_or(0);
        let ft_input_dimensions = parse_ft_input_dimensions(&architecture, threat_dimensions)?;
        let ft_hash = cursor.take(4, "FT hash")?;
        let (feature_transformer, ft_biases) = parse_leb128_ft(&mut cursor, ft_hash, l1)?;

        let psqt = if architecture.contains("PSQT=") {
            Some(TensorBinLayout {
                biases: cursor.take_count(num_buckets, 4, "PSQT biases")?,
                weights: cursor.take_count(
                    ft_input_dimensions
                        .checked_mul(num_buckets)
                        .ok_or_else(|| invalid("PSQT dimensions overflow"))?,
                    4,
                    "PSQT weights",
                )?,
            })
        } else {
            None
        };
        let threat_profile = if architecture.contains("ThreatProfile=") {
            Some(cursor.take(4, "Threat profile")?)
        } else {
            None
        };
        let threat_weights = if threat_dimensions == 0 {
            None
        } else {
            Some(
                cursor.take_count(
                    threat_dimensions
                        .checked_mul(l1)
                        .ok_or_else(|| invalid("Threat dimensions overflow"))?,
                    1,
                    "Threat weights",
                )?,
            )
        };

        let mut buckets = Vec::with_capacity(num_buckets);
        for bucket in 0..num_buckets {
            let fc_hash = cursor.take(4, &format!("bucket {bucket} FC hash"))?;
            let first = parse_affine(&mut cursor, l1, l2, "l1")?;
            let second_input = 2usize
                .checked_mul(l2 - 1)
                .ok_or_else(|| invalid("L2 input dimensions overflow"))?;
            let second = parse_affine(&mut cursor, second_input, l3, "l2")?;
            let output = parse_affine(&mut cursor, l3, 1, "output")?;
            buckets.push(LayerStackBucketBinLayout {
                fc_hash,
                l1: first,
                l2: second,
                output,
            });
        }
        let consumed = cursor.position;
        if consumed != cursor.file_size {
            return Err(invalid(format!(
                "unexpected trailing LayerStacks data: consumed={}, file_size={}",
                consumed, cursor.file_size
            )));
        }
        let fc_range = buckets
            .first()
            .zip(buckets.last())
            .map(|(first, last)| first.fc_hash.start..last.output.weights.end)
            .ok_or_else(|| invalid("LayerStacks has no FC buckets"))?;
        let fc_data = StoredBinBlock {
            bytes: cursor.read_range(fc_range.clone(), "FC blocks")?,
            source: fc_range,
        };
        Ok(Self {
            version,
            network_hash,
            architecture,
            num_buckets,
            l1,
            l2,
            l3,
            ft_input_dimensions,
            feature_transformer,
            psqt,
            threat_profile,
            threat_weights,
            buckets,
            ft_biases,
            fc_data,
        })
    }

    /// 対象 tensor のファイル格納上の形状を返す。
    pub fn tensor_shape(&self, kind: NetTensorKind) -> NetTensorShape {
        match kind {
            NetTensorKind::OutputWeight => NetTensorShape {
                bucket_count: Some(self.num_buckets),
                element_count: self.buckets[0].output.weights.len(),
            },
            NetTensorKind::OutputBias => NetTensorShape {
                bucket_count: Some(self.num_buckets),
                element_count: 1,
            },
            NetTensorKind::FtBias => NetTensorShape {
                bucket_count: None,
                element_count: self.l1,
            },
            NetTensorKind::L2Weight => NetTensorShape {
                bucket_count: Some(self.num_buckets),
                element_count: self.buckets[0].l2.weights.len(),
            },
        }
    }

    /// ファイル格納順 ID で指定した係数の現在値を返す。
    pub fn coefficient(&self, id: &NetCoefficientId) -> Result<i32, NetDeltaError> {
        self.tensor_shape(id.kind).validate(id)?;
        match id.kind {
            NetTensorKind::FtBias => Ok(i32::from(self.ft_biases[id.index])),
            NetTensorKind::OutputWeight => {
                let bucket = id.bucket.expect("validated bucket");
                self.fc_data.read_i8(self.buckets[bucket].output.weights.start + id.index)
            }
            NetTensorKind::OutputBias => {
                let bucket = id.bucket.expect("validated bucket");
                self.fc_data.read_i32(self.buckets[bucket].output.biases.start)
            }
            NetTensorKind::L2Weight => {
                let bucket = id.bucket.expect("validated bucket");
                self.fc_data.read_i8(self.buckets[bucket].l2.weights.start + id.index)
            }
        }
    }
}

impl StoredBinBlock {
    fn get(&self, offset: usize, width: usize) -> Result<&[u8], NetDeltaError> {
        let relative = offset
            .checked_sub(self.source.start)
            .ok_or_else(|| invalid_binary(format!("byte offset {offset}")))?;
        let end = relative
            .checked_add(width)
            .ok_or_else(|| invalid_binary("byte range overflow"))?;
        self.bytes
            .get(relative..end)
            .ok_or_else(|| invalid_binary(format!("byte offset {offset}")))
    }

    fn read_i8(&self, offset: usize) -> Result<i32, NetDeltaError> {
        Ok(i32::from(self.get(offset, 1)?[0] as i8))
    }

    fn read_i32(&self, offset: usize) -> Result<i32, NetDeltaError> {
        Ok(i32::from_le_bytes(
            self.get(offset, 4)?.try_into().map_err(|_| invalid_binary("truncated i32"))?,
        ))
    }
}

fn is_layer_stacks_architecture(architecture: &str) -> bool {
    matches!(
        super::spec::parse_feature_set_from_arch(architecture),
        Ok(FeatureSet::LayerStacks | FeatureSet::HalfKaHmMergedEffectBucket)
    )
}

fn parse_ft_input_dimensions(architecture: &str, threat: usize) -> io::Result<usize> {
    let reported = parse_feature_input_dimensions(architecture)
        .ok_or_else(|| invalid("missing FT input dimensions"))?;
    let feature = parse_layer_stacks_feature_set_keyword(architecture)
        .map_err(invalid)?
        .ok_or_else(|| invalid("LayerStacks header does not identify its FT feature set"))?;
    let dimensions = if architecture.contains("EffectBucket=") || architecture.contains("E4=") {
        parse_effect_config(architecture)
            .ok_or_else(|| invalid("malformed EffectBucket token"))?
            .dimensions()
    } else {
        match feature {
            FeatureSet::HalfKP => HALFKP_DIMENSIONS,
            FeatureSet::HalfKaHmMerged => HALFKA_HM_DIMENSIONS,
            FeatureSet::HalfKaSplit => HALFKA_DIMENSIONS,
            FeatureSet::HalfKaMerged => HALFKA_MERGED_DIMENSIONS,
            FeatureSet::HalfKaHmSplit => HALFKA_HM_SPLIT_DIMENSIONS,
            FeatureSet::HalfKaHmMergedEffectBucket | FeatureSet::LayerStacks => {
                return Err(invalid("unsupported LayerStacks FT feature set"));
            }
        }
    };
    if reported == dimensions || (threat != 0 && dimensions.checked_add(threat) == Some(reported)) {
        Ok(dimensions)
    } else {
        Err(invalid(format!(
            "FT input dimension mismatch: header={reported}, runtime={dimensions}, threat={threat}"
        )))
    }
}

fn parse_effect_config(architecture: &str) -> Option<EffectBucketConfig> {
    let token = architecture
        .split(',')
        .find_map(|part| part.strip_prefix("EffectBucket=").or_else(|| part.strip_prefix("E4=")))?;
    match token {
        "2x2fixed" | "4xfixed" => Some(EffectBucketConfig::KINGFIXED_2X2),
        "2x2bucketed" | "4xbucketed" => Some(EffectBucketConfig::KINGBUCKETED_2X2),
        "3x3fixed" | "9xfixed" => Some(EffectBucketConfig::KINGFIXED_3X3),
        "3x3bucketed" | "9xbucketed" => Some(EffectBucketConfig::KINGBUCKETED_3X3),
        _ => None,
    }
}

fn parse_leb128_ft<R: Read + Seek>(
    cursor: &mut StreamCursor<'_, R>,
    hash: Range<usize>,
    bias_count: usize,
) -> io::Result<(FeatureTransformerBinLayout, Vec<i16>)> {
    let first = cursor.take_leb128_prefix("FT first LEB128 block", bias_count)?;
    if first.prefix_end == first.data.end {
        let weights = cursor.take_leb128_payload("FT weights LEB128 block")?;
        Ok((
            FeatureTransformerBinLayout {
                hash,
                encoding: FtBinEncoding::Leb128Split,
                biases: first.data,
                weights,
            },
            first.values,
        ))
    } else {
        Ok((
            FeatureTransformerBinLayout {
                hash,
                encoding: FtBinEncoding::Leb128Combined,
                biases: first.data.start..first.prefix_end,
                weights: first.prefix_end..first.data.end,
            },
            first.values,
        ))
    }
}

fn parse_affine<R: Read + Seek>(
    cursor: &mut StreamCursor<'_, R>,
    input: usize,
    output: usize,
    name: &str,
) -> io::Result<TensorBinLayout> {
    Ok(TensorBinLayout {
        biases: cursor.take_count(output, 4, &format!("{name} biases"))?,
        weights: cursor.take_count(
            output
                .checked_mul(checked_padded_input(input)?)
                .ok_or_else(|| invalid(format!("{name} dimensions overflow")))?,
            1,
            &format!("{name} weights"),
        )?,
    })
}

fn checked_padded_input(input: usize) -> io::Result<usize> {
    input
        .checked_add(31)
        .map(|value| value / 32 * 32)
        .ok_or_else(|| invalid("padded input dimensions overflow"))
}

struct Leb128Block {
    data: Range<usize>,
    prefix_end: usize,
    values: Vec<i16>,
}

struct StreamCursor<'a, R> {
    reader: &'a mut R,
    position: usize,
    file_size: usize,
}

impl<'a, R: Read + Seek> StreamCursor<'a, R> {
    fn new(reader: &'a mut R) -> io::Result<Self> {
        let position = usize_from_u64(reader.stream_position()?, "stream position")?;
        if position != 0 {
            return Err(invalid(format!(
                "LayerStacks reader must start at byte 0, got {position}"
            )));
        }
        let file_size = usize_from_u64(reader.seek(SeekFrom::End(0))?, "file size")?;
        reader.seek(SeekFrom::Start(0))?;
        Ok(Self {
            reader,
            position: 0,
            file_size,
        })
    }

    fn take(&mut self, size: usize, name: &str) -> io::Result<Range<usize>> {
        let end = self
            .position
            .checked_add(size)
            .ok_or_else(|| invalid(format!("{name} range overflow")))?;
        if end > self.file_size {
            return Err(invalid(format!("truncated {name}")));
        }
        let range = self.position..end;
        self.reader.seek(SeekFrom::Start(u64_from_usize(end, name)?))?;
        self.position = end;
        Ok(range)
    }

    fn read_bytes(&mut self, size: usize, name: &str) -> io::Result<Vec<u8>> {
        let end = self
            .position
            .checked_add(size)
            .ok_or_else(|| invalid(format!("{name} range overflow")))?;
        if end > self.file_size {
            return Err(invalid(format!("truncated {name}")));
        }
        let mut bytes = vec![0u8; size];
        self.reader.read_exact(&mut bytes)?;
        self.position = end;
        Ok(bytes)
    }

    fn read_array<const N: usize>(&mut self, name: &str) -> io::Result<[u8; N]> {
        let end = self
            .position
            .checked_add(N)
            .ok_or_else(|| invalid(format!("{name} range overflow")))?;
        if end > self.file_size {
            return Err(invalid(format!("truncated {name}")));
        }
        let mut bytes = [0u8; N];
        self.reader.read_exact(&mut bytes)?;
        self.position = end;
        Ok(bytes)
    }

    fn read_range(&mut self, range: Range<usize>, name: &str) -> io::Result<Vec<u8>> {
        if range.start > range.end || range.end > self.file_size {
            return Err(invalid(format!("invalid {name} range")));
        }
        self.reader.seek(SeekFrom::Start(u64_from_usize(range.start, name)?))?;
        self.position = range.start;
        self.read_bytes(range.len(), name)
    }

    fn take_count(&mut self, count: usize, width: usize, name: &str) -> io::Result<Range<usize>> {
        let size = count
            .checked_mul(width)
            .ok_or_else(|| invalid(format!("{name} size overflow")))?;
        self.take(size, name)
    }

    fn read_u32(&mut self, name: &str) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.read_array(name)?))
    }

    fn take_leb128_payload(&mut self, name: &str) -> io::Result<Range<usize>> {
        let magic: [u8; 17] = self.read_array(name)?;
        if magic != LEB128_MAGIC {
            return Err(invalid(format!("{name} has invalid magic")));
        }
        let size = self.read_u32(name)? as usize;
        if size == 0 || size > MAX_COMPRESSED_SIZE {
            return Err(invalid(format!(
                "invalid {name} size: {size} (max: {MAX_COMPRESSED_SIZE})"
            )));
        }
        self.take(size, name)
    }

    fn take_leb128_prefix(&mut self, name: &str, prefix_count: usize) -> io::Result<Leb128Block> {
        let magic: [u8; 17] = self.read_array(name)?;
        if magic != LEB128_MAGIC {
            return Err(invalid("unsupported FT encoding: expected COMPRESSED_LEB128 magic"));
        }
        let size = self.read_u32(name)? as usize;
        if size == 0 || size > MAX_COMPRESSED_SIZE {
            return Err(invalid(format!(
                "invalid {name} size: {size} (max: {MAX_COMPRESSED_SIZE})"
            )));
        }
        let data_start = self.position;
        let data_end = data_start
            .checked_add(size)
            .ok_or_else(|| invalid(format!("{name} range overflow")))?;
        if data_end > self.file_size {
            return Err(invalid(format!("truncated {name}")));
        }
        let mut values = Vec::with_capacity(prefix_count);
        for _ in 0..prefix_count {
            values.push(self.read_leb128_i16(data_end)?);
        }
        let prefix_end = self.position;
        self.reader.seek(SeekFrom::Start(u64_from_usize(data_end, name)?))?;
        self.position = data_end;
        Ok(Leb128Block {
            data: data_start..data_end,
            prefix_end,
            values,
        })
    }

    fn read_leb128_i16(&mut self, block_end: usize) -> io::Result<i16> {
        let mut result = 0i64;
        let mut shift = 0u32;
        loop {
            if self.position >= block_end {
                return Err(invalid("truncated FT bias LEB128 value"));
            }
            let byte = self.read_array::<1>("FT bias LEB128 value")?[0];
            result |= i64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    result |= !0i64 << shift;
                }
                return i16::try_from(result)
                    .map_err(|_| invalid(format!("FT bias is outside i16 range: {result}")));
            }
            if shift >= 64 {
                return Err(invalid("FT bias LEB128 value overflow"));
            }
        }
    }
}

fn usize_from_u64(value: u64, name: &str) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| invalid(format!("{name} exceeds usize")))
}

fn u64_from_usize(value: usize, name: &str) -> io::Result<u64> {
    u64::try_from(value).map_err(|_| invalid(format!("{name} offset exceeds u64")))
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn invalid_binary(message: impl Into<String>) -> NetDeltaError {
    NetDeltaError::InvalidBinary {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::features::{FeatureSet as FeatureSetTrait, HalfKPFeatureSet};
    use crate::nnue::net_delta::test_utils::{
        SyntheticFtEncoding, build_synthetic_layer_stacks_with_ft_encoding,
    };
    #[cfg(feature = "nnue-runtime-dimensions")]
    use crate::nnue::network::NNUENetwork;

    fn id(kind: NetTensorKind, bucket: Option<usize>, index: usize) -> NetCoefficientId {
        NetCoefficientId {
            kind,
            bucket,
            index,
        }
    }

    fn replace_architecture(bytes: &[u8], architecture: &str) -> Vec<u8> {
        let old_length = u32::from_le_bytes(bytes[8..12].try_into().expect("architecture length"));
        let suffix_start = 12 + old_length as usize;
        let mut replaced = Vec::new();
        replaced.extend_from_slice(&bytes[..8]);
        replaced.extend_from_slice(&(architecture.len() as u32).to_le_bytes());
        replaced.extend_from_slice(architecture.as_bytes());
        replaced.extend_from_slice(&bytes[suffix_start..]);
        replaced
    }

    fn with_psqt_and_threat(bytes: &[u8]) -> Vec<u8> {
        let original = LayerStacksBinLayout::from_bytes(bytes).expect("original");
        let architecture = format!(
            "Features=HalfKP[{}->32x2],LayerStacks,l2=4,l3=3,PSQT=2,Threat=5,ThreatProfile=0",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS
        );
        let mut extended = Vec::new();
        extended.extend_from_slice(&original.version.to_le_bytes());
        extended.extend_from_slice(&original.network_hash.to_le_bytes());
        extended.extend_from_slice(&(architecture.len() as u32).to_le_bytes());
        extended.extend_from_slice(architecture.as_bytes());
        extended.extend_from_slice(&(original.num_buckets as u32).to_le_bytes());
        extended.extend_from_slice(
            &bytes[original.feature_transformer.hash.start..original.buckets[0].fc_hash.start],
        );
        extended.resize(extended.len() + original.num_buckets * 4, 0);
        extended
            .resize(extended.len() + original.ft_input_dimensions * original.num_buckets * 4, 0);
        extended.extend_from_slice(&0u32.to_le_bytes());
        extended.resize(extended.len() + 5 * original.l1, 0);
        extended.extend_from_slice(&bytes[original.buckets[0].fc_hash.start..]);
        extended
    }

    struct ReadAudit {
        cursor: Cursor<Vec<u8>>,
        reads: Vec<Range<usize>>,
    }

    impl Read for ReadAudit {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let start = usize_from_u64(self.cursor.position(), "audit position")?;
            let read = self.cursor.read(buffer)?;
            if read != 0 {
                self.reads.push(start..start + read);
            }
            Ok(read)
        }
    }

    impl Seek for ReadAudit {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.cursor.seek(position)
        }
    }

    #[test]
    #[cfg(feature = "nnue-runtime-dimensions")]
    fn layout_coefficients_match_dynamic_network_for_leb128() {
        for (num_buckets, encoding) in [
            (2, SyntheticFtEncoding::Leb128Combined),
            (3, SyntheticFtEncoding::Leb128Split),
        ] {
            let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
                "HalfKP",
                <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
                32,
                4,
                3,
                num_buckets,
                encoding,
            );
            let layout = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("layout");
            let network = NNUENetwork::from_bytes(&synthetic.bytes).expect("dynamic network");
            assert_eq!(layout.num_buckets, num_buckets);
            assert_eq!(layout.l1, 32);
            assert_eq!(layout.l2, 4);
            assert_eq!(layout.l3, 3);
            assert_eq!(
                layout.ft_input_dimensions,
                <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS
            );

            let cases = [
                id(NetTensorKind::OutputWeight, Some(0), 0),
                id(NetTensorKind::OutputWeight, Some(num_buckets - 1), 31),
                id(NetTensorKind::OutputBias, Some(0), 0),
                id(NetTensorKind::OutputBias, Some(num_buckets - 1), 0),
                id(NetTensorKind::FtBias, None, 0),
                id(NetTensorKind::FtBias, None, 17),
                id(NetTensorKind::L2Weight, Some(0), 2),
                id(NetTensorKind::L2Weight, Some(num_buckets - 1), 95),
            ];
            for coefficient in cases {
                assert_eq!(
                    layout.coefficient(&coefficient).expect("layout coefficient"),
                    network.net_coefficient(&coefficient).expect("network coefficient"),
                    "{coefficient:?}, encoding={encoding:?}"
                );
            }
        }
    }

    #[test]
    fn rejects_non_leb128_ft_encoding() {
        let mut synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Split,
        );
        let original = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("original");
        synthetic.bytes[original.feature_transformer.hash.end] = 0;

        let error = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect_err("unsupported FT");
        assert!(error.to_string().contains("unsupported FT encoding"));

        #[cfg(feature = "nnue-runtime-dimensions")]
        {
            let error = match NNUENetwork::from_bytes(&synthetic.bytes) {
                Ok(_) => panic!("unsupported FT was accepted"),
                Err(error) => error,
            };
            assert_eq!(error.to_string(), "Expected COMPRESSED_LEB128 magic");
        }
    }

    #[test]
    fn layout_skips_ft_weight_payload() {
        for encoding in [
            SyntheticFtEncoding::Leb128Combined,
            SyntheticFtEncoding::Leb128Split,
        ] {
            let mut synthetic = build_synthetic_layer_stacks_with_ft_encoding(
                "HalfKP",
                <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
                32,
                4,
                3,
                2,
                encoding,
            );
            let original = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("original");
            let original_bias = original
                .coefficient(&id(NetTensorKind::FtBias, None, 0))
                .expect("original FT bias");
            synthetic.bytes[original.feature_transformer.weights.clone()].fill(0x80);

            let mut reader = ReadAudit {
                cursor: Cursor::new(synthetic.bytes),
                reads: Vec::new(),
            };
            let layout = LayerStacksBinLayout::from_reader(&mut reader).expect("layout");
            assert_eq!(layout.feature_transformer.weights, original.feature_transformer.weights);
            assert!(reader.reads.iter().all(|read| {
                read.end <= layout.feature_transformer.weights.start
                    || read.start >= layout.feature_transformer.weights.end
            }));
            assert_eq!(
                layout.coefficient(&id(NetTensorKind::FtBias, None, 0)).expect("FT bias"),
                original_bias
            );
        }
    }

    #[test]
    fn retained_data_is_limited_to_ft_biases_and_fc_blocks() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            3,
            SyntheticFtEncoding::Leb128Combined,
        );
        let mut reader = Cursor::new(&synthetic.bytes);
        let from_reader = LayerStacksBinLayout::from_reader(&mut reader).expect("reader layout");
        let from_bytes = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("bytes layout");

        assert_eq!(from_reader, from_bytes);
        assert_eq!(from_reader.ft_biases.len(), from_reader.l1);
        assert_eq!(
            from_reader.fc_data.bytes.len(),
            from_reader.buckets.last().expect("last bucket").output.weights.end
                - from_reader.buckets[0].fc_hash.start
        );
        assert!(
            from_reader.fc_data.bytes.len()
                + from_reader.ft_biases.len() * std::mem::size_of::<i16>()
                < from_reader.feature_transformer.weights.len()
        );
    }

    #[test]
    fn rejects_non_layer_stacks_and_trailing_data() {
        let mut non_layer_stacks = Vec::new();
        let arch = "Features=HalfKP[125388->32x2],l2=4,l3=3";
        non_layer_stacks.extend_from_slice(&NNUE_VERSION_HALFKA.to_le_bytes());
        non_layer_stacks.extend_from_slice(&0u32.to_le_bytes());
        non_layer_stacks.extend_from_slice(&(arch.len() as u32).to_le_bytes());
        non_layer_stacks.extend_from_slice(arch.as_bytes());
        assert!(LayerStacksBinLayout::from_bytes(&non_layer_stacks).is_err());

        let mut synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Split,
        )
        .bytes;
        synthetic.push(0);
        let error = LayerStacksBinLayout::from_bytes(&synthetic).expect_err("trailing byte");
        assert!(error.to_string().contains("trailing"));
    }

    #[test]
    fn rejects_loader_invalid_architecture_headers() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Split,
        );
        for suffix in [",Threat=0", ",Factorizer"] {
            let original = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("original");
            let architecture = format!("{}{suffix}", original.architecture);
            let bytes = replace_architecture(&synthetic.bytes, &architecture);
            assert!(LayerStacksBinLayout::from_bytes(&bytes).is_err(), "{suffix}");
        }
    }

    #[test]
    fn layout_skips_psqt_and_threat_payloads() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Split,
        );
        let bytes = with_psqt_and_threat(&synthetic.bytes);
        let expected = LayerStacksBinLayout::from_bytes(&bytes).expect("expected layout");
        let mut reader = ReadAudit {
            cursor: Cursor::new(bytes),
            reads: Vec::new(),
        };
        let actual = LayerStacksBinLayout::from_reader(&mut reader).expect("audited layout");
        assert_eq!(actual, expected);

        let psqt = actual.psqt.expect("PSQT");
        let skipped = [
            psqt.biases,
            psqt.weights,
            actual.threat_weights.expect("threat weights"),
        ];
        for payload in skipped {
            assert!(
                reader
                    .reads
                    .iter()
                    .all(|read| read.end <= payload.start || read.start >= payload.end),
                "payload was read: {payload:?}"
            );
        }
    }

    #[test]
    fn locates_optional_psqt_and_threat_blocks() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Split,
        );
        let bytes = with_psqt_and_threat(&synthetic.bytes);

        let layout = LayerStacksBinLayout::from_bytes(&bytes).expect("extended layout");
        let psqt = layout.psqt.expect("PSQT");
        assert_eq!(psqt.biases.len(), layout.num_buckets * 4);
        assert_eq!(psqt.weights.len(), layout.ft_input_dimensions * layout.num_buckets * 4);
        assert_eq!(layout.threat_profile.expect("profile").len(), 4);
        assert_eq!(layout.threat_weights.expect("threat").len(), 5 * layout.l1);
    }
}
