//! LayerStacks `.bin` の feature 非依存レイアウト走査。

use std::fmt;
use std::io;
use std::ops::Range;

use super::bona_piece_effect_bucket::EffectBucketConfig;
use super::constants::{
    DEFAULT_NUM_BUCKETS, HALFKA_DIMENSIONS, HALFKA_HM_DIMENSIONS, HALFKA_HM_SPLIT_DIMENSIONS,
    HALFKA_MERGED_DIMENSIONS, HALFKP_DIMENSIONS, MAX_ARCH_LEN, MAX_LAYER_STACK_BUCKETS,
    NNUE_VERSION_HALFKA, NNUE_VERSION_LAYERSTACK_NUM_BUCKETS,
};
use super::leb128::{
    LEB128_MAGIC, MAX_COMPRESSED_SIZE, decode_single_leb128, encode_signed_leb128,
};
use super::net_delta::{
    NetCoefficientId, NetDelta, NetDeltaError, NetDeltaReport, NetTensorKind, NetTensorShape,
    add_i8_delta, add_i16_delta, add_i32_delta,
};
use super::spec::{
    FeatureSet, parse_arch_dimensions, parse_feature_input_dimensions,
    parse_layer_stacks_feature_set_keyword,
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

/// LEB128 FT bias の decode 結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFtBiases {
    source: Range<usize>,
    values: Vec<i16>,
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
}

/// LayerStacks `.bin` の書換えエラー。
#[derive(Debug)]
pub enum NetBinPatchError {
    /// `.bin` レイアウトの読み取りに失敗した。
    Io(io::Error),
    /// delta の ID または対象バイト列が不正だった。
    Delta(NetDeltaError),
}

impl fmt::Display for NetBinPatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Delta(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for NetBinPatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Delta(error) => Some(error),
        }
    }
}

impl From<io::Error> for NetBinPatchError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<NetDeltaError> for NetBinPatchError {
    fn from(error: NetDeltaError) -> Self {
        Self::Delta(error)
    }
}

/// LayerStacks `.bin` の指定係数へ engine と同じ saturating delta を適用する。
///
/// 編集対象外の領域は入力からそのままコピーする。LEB128 FT は元の Combined / Split
/// ブロック構成を維持し、変更を含むブロックだけを最短形で再エンコードする。
pub fn apply_deltas_to_bytes(
    bytes: &[u8],
    deltas: &[NetDelta],
) -> Result<(Vec<u8>, NetDeltaReport), NetBinPatchError> {
    let layout = LayerStacksBinLayout::from_bytes(bytes)?;
    for delta in deltas {
        layout.tensor_shape(delta.id.kind).validate(&delta.id)?;
    }
    let has_nonzero_ft_delta = deltas
        .iter()
        .any(|delta| delta.id.kind == NetTensorKind::FtBias && delta.delta != 0);
    if layout.feature_transformer.encoding == FtBinEncoding::Leb128Combined && !has_nonzero_ft_delta
    {
        validate_combined_ft_payload(bytes, &layout)?;
    }

    let mut report = NetDeltaReport {
        applied: deltas.len(),
        clamped: 0,
    };
    if deltas.iter().all(|delta| delta.delta == 0) {
        return Ok((bytes.to_vec(), report));
    }

    let mut patched = bytes.to_vec();
    let mut ft_values =
        has_nonzero_ft_delta.then(|| decode_ft_tensor(bytes, &layout)).transpose()?;
    for delta in deltas {
        if delta.delta == 0 {
            continue;
        }
        let clamped = match delta.id.kind {
            NetTensorKind::FtBias => {
                let values = ft_values
                    .as_mut()
                    .ok_or_else(|| invalid_binary("FT values were not decoded"))?;
                let current = values
                    .get_mut(delta.id.index)
                    .ok_or_else(|| invalid_binary("validated FT bias index is missing"))?;
                let (value, clamped) = add_i16_delta(*current, delta.delta);
                *current = value;
                clamped
            }
            NetTensorKind::OutputWeight => {
                let bucket = validated_bucket(&layout, &delta.id)?;
                patch_i8(&mut patched, bucket.output.weights.start + delta.id.index, delta.delta)?
            }
            NetTensorKind::OutputBias => {
                let bucket = validated_bucket(&layout, &delta.id)?;
                patch_i32(&mut patched, bucket.output.biases.start, delta.delta)?
            }
            NetTensorKind::L2Weight => {
                let bucket = validated_bucket(&layout, &delta.id)?;
                patch_i8(&mut patched, bucket.l2.weights.start + delta.id.index, delta.delta)?
            }
        };
        report.clamped += usize::from(clamped);
    }

    if let Some(values) = ft_values {
        patched = replace_ft_block(&patched, &layout, &values)?;
    }
    Ok((patched, report))
}

fn validated_bucket<'a>(
    layout: &'a LayerStacksBinLayout,
    id: &NetCoefficientId,
) -> Result<&'a LayerStackBucketBinLayout, NetDeltaError> {
    let bucket = id.bucket.ok_or_else(|| NetDeltaError::MissingBucket {
        name: id.usi_name(),
    })?;
    layout
        .buckets
        .get(bucket)
        .ok_or_else(|| invalid_binary("validated bucket is missing from layout"))
}

impl LayerStacksBinLayout {
    /// `.bin` 全体を走査し、LayerStacks の各 byte 範囲を返す。
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        let mut cursor = ByteCursor::new(bytes);
        let version = cursor.read_u32("version")?;
        if version != NNUE_VERSION_HALFKA && version != NNUE_VERSION_LAYERSTACK_NUM_BUCKETS {
            return Err(invalid(format!("unsupported LayerStacks version {version:#x}")));
        }
        let network_hash = cursor.read_u32("network hash")?;
        let arch_len = cursor.read_u32("architecture length")? as usize;
        if arch_len == 0 || arch_len > MAX_ARCH_LEN {
            return Err(invalid(format!("invalid architecture string length: {arch_len}")));
        }
        let architecture_range = cursor.take(arch_len, "architecture string")?;
        let architecture = std::str::from_utf8(&bytes[architecture_range])
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
        let threat_dimensions = parse_token_usize(&architecture, "Threat=")?;
        let ft_input_dimensions = parse_ft_input_dimensions(&architecture, threat_dimensions)?;
        let ft_hash = cursor.take(4, "FT hash")?;
        if !cursor.remaining().starts_with(LEB128_MAGIC) {
            return Err(invalid("unsupported FT encoding: expected COMPRESSED_LEB128 magic"));
        }
        let feature_transformer = parse_leb128_ft(&mut cursor, ft_hash, l1)?;

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
        if cursor.position != bytes.len() {
            return Err(invalid(format!(
                "unexpected trailing LayerStacks data: consumed={}, file_size={}",
                cursor.position,
                bytes.len()
            )));
        }
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

    /// FT bias を必要な部分だけ decode し、繰り返し参照用の cache を返す。
    pub fn decode_ft_biases(&self, bytes: &[u8]) -> Result<DecodedFtBiases, NetDeltaError> {
        let range = self.feature_transformer.biases.clone();
        let encoded = bytes.get(range.clone()).ok_or_else(|| invalid_binary("FT bias range"))?;
        let values = decode_i16_values(encoded, self.l1)
            .map_err(|error| invalid_binary(error.to_string()))?;
        Ok(DecodedFtBiases {
            source: range,
            values,
        })
    }

    /// ファイル格納順 ID で指定した係数の現在値を返す。
    pub fn coefficient(&self, bytes: &[u8], id: &NetCoefficientId) -> Result<i32, NetDeltaError> {
        self.coefficient_with_ft_biases(bytes, id, None)
    }

    /// decode 済み FT bias cache を再利用して係数の現在値を返す。
    pub fn coefficient_with_ft_biases(
        &self,
        bytes: &[u8],
        id: &NetCoefficientId,
        ft_biases: Option<&DecodedFtBiases>,
    ) -> Result<i32, NetDeltaError> {
        self.tensor_shape(id.kind).validate(id)?;
        match id.kind {
            NetTensorKind::FtBias => {
                if let Some(decoded) = ft_biases {
                    if decoded.source != self.feature_transformer.biases {
                        return Err(invalid_binary("FT bias cache does not match layout"));
                    }
                    return Ok(i32::from(decoded.values[id.index]));
                }
                let decoded = self.decode_ft_biases(bytes)?;
                Ok(i32::from(decoded.values[id.index]))
            }
            NetTensorKind::OutputWeight => {
                let bucket = id.bucket.expect("validated bucket");
                read_i8(bytes, self.buckets[bucket].output.weights.start + id.index)
            }
            NetTensorKind::OutputBias => {
                let bucket = id.bucket.expect("validated bucket");
                read_i32(bytes, self.buckets[bucket].output.biases.start)
            }
            NetTensorKind::L2Weight => {
                let bucket = id.bucket.expect("validated bucket");
                read_i8(bytes, self.buckets[bucket].l2.weights.start + id.index)
            }
        }
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

fn parse_token_usize(architecture: &str, token: &str) -> io::Result<usize> {
    match architecture.split(',').find_map(|part| part.strip_prefix(token)) {
        Some(value) => value.parse().map_err(|_| invalid(format!("malformed {token} token"))),
        None => Ok(0),
    }
}

fn parse_leb128_ft(
    cursor: &mut ByteCursor<'_>,
    hash: Range<usize>,
    bias_count: usize,
) -> io::Result<FeatureTransformerBinLayout> {
    let first = cursor.take_leb128_prefix("FT first LEB128 block", bias_count)?;
    if first.prefix_end == first.data.end {
        let weights = cursor.take_leb128_payload("FT weights LEB128 block")?;
        Ok(FeatureTransformerBinLayout {
            hash,
            encoding: FtBinEncoding::Leb128Split,
            biases: first.data,
            weights,
        })
    } else {
        Ok(FeatureTransformerBinLayout {
            hash,
            encoding: FtBinEncoding::Leb128Combined,
            biases: first.data.start..first.prefix_end,
            weights: first.prefix_end..first.data.end,
        })
    }
}

fn parse_affine(
    cursor: &mut ByteCursor<'_>,
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
}

struct ByteCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ByteCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.position..]
    }

    fn take(&mut self, size: usize, name: &str) -> io::Result<Range<usize>> {
        let end = self
            .position
            .checked_add(size)
            .ok_or_else(|| invalid(format!("{name} range overflow")))?;
        if end > self.bytes.len() {
            return Err(invalid(format!("truncated {name}")));
        }
        let range = self.position..end;
        self.position = end;
        Ok(range)
    }

    fn take_count(&mut self, count: usize, width: usize, name: &str) -> io::Result<Range<usize>> {
        let size = count
            .checked_mul(width)
            .ok_or_else(|| invalid(format!("{name} size overflow")))?;
        self.take(size, name)
    }

    fn read_u32(&mut self, name: &str) -> io::Result<u32> {
        let range = self.take(4, name)?;
        Ok(u32::from_le_bytes(
            self.bytes[range].try_into().map_err(|_| invalid(format!("truncated {name}")))?,
        ))
    }

    fn take_leb128_payload(&mut self, name: &str) -> io::Result<Range<usize>> {
        let magic = self.take(LEB128_MAGIC.len(), name)?;
        if &self.bytes[magic] != LEB128_MAGIC {
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
        let data = self.take_leb128_payload(name)?;
        let mut position = data.start;
        for _ in 0..prefix_count {
            let (_, consumed) = decode_single_leb128(&self.bytes[position..data.end])?;
            position += consumed;
        }
        Ok(Leb128Block {
            data,
            prefix_end: position,
        })
    }
}

fn decode_i16_values(bytes: &[u8], expected: usize) -> io::Result<Vec<i16>> {
    let mut values = Vec::with_capacity(expected);
    let mut position = 0usize;
    while position < bytes.len() {
        let (value, consumed) = decode_single_leb128(&bytes[position..])?;
        values.push(i16::try_from(value).map_err(|_| {
            invalid(format!("LEB128 value is outside i16 range at byte {position}: {value}"))
        })?);
        position += consumed;
    }
    if values.len() != expected {
        return Err(invalid(format!(
            "FT bias count mismatch: got {}, expected {expected}",
            values.len()
        )));
    }
    Ok(values)
}

fn decode_ft_tensor(
    bytes: &[u8],
    layout: &LayerStacksBinLayout,
) -> Result<Vec<i16>, NetDeltaError> {
    let range = match layout.feature_transformer.encoding {
        FtBinEncoding::Leb128Combined => {
            layout.feature_transformer.biases.start..layout.feature_transformer.weights.end
        }
        FtBinEncoding::Leb128Split => layout.feature_transformer.biases.clone(),
    };
    let encoded = bytes.get(range).ok_or_else(|| invalid_binary("FT tensor range"))?;
    let expected = match layout.feature_transformer.encoding {
        FtBinEncoding::Leb128Combined => layout
            .l1
            .checked_add(
                layout
                    .ft_input_dimensions
                    .checked_mul(layout.l1)
                    .ok_or_else(|| invalid_binary("FT dimensions overflow"))?,
            )
            .ok_or_else(|| invalid_binary("FT dimensions overflow"))?,
        FtBinEncoding::Leb128Split => layout.l1,
    };
    let values =
        decode_i16_values(encoded, expected).map_err(|error| invalid_binary(error.to_string()))?;
    if layout.feature_transformer.encoding == FtBinEncoding::Leb128Combined
        && !is_canonical_i16_leb128(encoded, &values)
    {
        return Err(invalid_binary("Combined FT LEB128 payload is not canonical"));
    }
    Ok(values)
}

fn validate_combined_ft_payload(
    bytes: &[u8],
    layout: &LayerStacksBinLayout,
) -> Result<(), NetDeltaError> {
    let range = layout.feature_transformer.biases.start..layout.feature_transformer.weights.end;
    let encoded = bytes.get(range).ok_or_else(|| invalid_binary("FT tensor range"))?;
    let expected = layout
        .l1
        .checked_add(
            layout
                .ft_input_dimensions
                .checked_mul(layout.l1)
                .ok_or_else(|| invalid_binary("FT dimensions overflow"))?,
        )
        .ok_or_else(|| invalid_binary("FT dimensions overflow"))?;
    let mut position = 0usize;
    let mut canonical = Vec::with_capacity(3);
    for _ in 0..expected {
        let (raw, consumed) = decode_single_leb128(&encoded[position..])
            .map_err(|error| invalid_binary(error.to_string()))?;
        let value = i16::try_from(raw).map_err(|_| {
            invalid_binary(format!("LEB128 value is outside i16 range at byte {position}: {raw}"))
        })?;
        canonical.clear();
        encode_signed_leb128(i64::from(value), &mut canonical);
        if encoded.get(position..position + consumed) != Some(canonical.as_slice()) {
            return Err(invalid_binary("Combined FT LEB128 payload is not canonical"));
        }
        position += consumed;
    }
    if position != encoded.len() {
        return Err(invalid_binary(format!(
            "FT element count mismatch: decoded {expected}, payload has trailing bytes"
        )));
    }
    Ok(())
}

fn is_canonical_i16_leb128(encoded: &[u8], values: &[i16]) -> bool {
    let mut position = 0usize;
    let mut canonical = Vec::with_capacity(3);
    for &value in values {
        canonical.clear();
        encode_signed_leb128(i64::from(value), &mut canonical);
        let Some(end) = position.checked_add(canonical.len()) else {
            return false;
        };
        if encoded.get(position..end) != Some(canonical.as_slice()) {
            return false;
        }
        position = end;
    }
    position == encoded.len()
}

fn replace_ft_block(
    bytes: &[u8],
    layout: &LayerStacksBinLayout,
    values: &[i16],
) -> Result<Vec<u8>, NetBinPatchError> {
    let payload = match layout.feature_transformer.encoding {
        FtBinEncoding::Leb128Combined => {
            layout.feature_transformer.biases.start..layout.feature_transformer.weights.end
        }
        FtBinEncoding::Leb128Split => layout.feature_transformer.biases.clone(),
    };
    let block_start = payload
        .start
        .checked_sub(LEB128_MAGIC.len() + 4)
        .ok_or_else(|| invalid_binary("FT block header range"))?;
    let mut encoded = Vec::new();
    for &value in values {
        encode_signed_leb128(i64::from(value), &mut encoded);
    }
    let encoded_size =
        u32::try_from(encoded.len()).map_err(|_| invalid("encoded FT block exceeds u32 size"))?;
    let capacity = bytes
        .len()
        .checked_sub(payload.end - block_start)
        .and_then(|size| size.checked_add(LEB128_MAGIC.len() + 4))
        .and_then(|size| size.checked_add(encoded.len()))
        .ok_or_else(|| invalid("patched NNUE size overflow"))?;
    let mut output = Vec::with_capacity(capacity);
    output.extend_from_slice(&bytes[..block_start]);
    output.extend_from_slice(LEB128_MAGIC);
    output.extend_from_slice(&encoded_size.to_le_bytes());
    output.extend_from_slice(&encoded);
    output.extend_from_slice(&bytes[payload.end..]);
    Ok(output)
}

fn patch_i8(bytes: &mut [u8], offset: usize, delta: i32) -> Result<bool, NetDeltaError> {
    let current = bytes
        .get(offset)
        .copied()
        .ok_or_else(|| invalid_binary(format!("byte offset {offset}")))? as i8;
    let (value, clamped) = add_i8_delta(current, delta);
    bytes[offset] = value as u8;
    Ok(clamped)
}

fn patch_i32(bytes: &mut [u8], offset: usize, delta: i32) -> Result<bool, NetDeltaError> {
    let end = offset.checked_add(4).ok_or_else(|| invalid_binary("i32 range overflow"))?;
    let current = bytes
        .get(offset..end)
        .ok_or_else(|| invalid_binary(format!("i32 offset {offset}")))?;
    let current =
        i32::from_le_bytes(current.try_into().map_err(|_| invalid_binary("truncated i32"))?);
    let (value, clamped) = add_i32_delta(current, delta);
    bytes[offset..end].copy_from_slice(&value.to_le_bytes());
    Ok(clamped)
}

fn read_i8(bytes: &[u8], offset: usize) -> Result<i32, NetDeltaError> {
    bytes
        .get(offset)
        .map(|value| i32::from(*value as i8))
        .ok_or_else(|| invalid_binary(format!("byte offset {offset}")))
}

fn read_i32(bytes: &[u8], offset: usize) -> Result<i32, NetDeltaError> {
    let end = offset.checked_add(4).ok_or_else(|| invalid_binary("i32 range overflow"))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| invalid_binary(format!("i32 offset {offset}")))?;
    Ok(i32::from_le_bytes(
        value.try_into().map_err(|_| invalid_binary("truncated i32"))?,
    ))
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
    #[cfg(feature = "nnue-runtime-dimensions")]
    use std::sync::Arc;

    use super::*;
    #[cfg(feature = "nnue-runtime-dimensions")]
    use crate::nnue::evaluator::NNUEEvaluator;
    use crate::nnue::features::{FeatureSet as FeatureSetTrait, HalfKPFeatureSet};
    use crate::nnue::net_delta::test_utils::{
        SyntheticFtConfig, SyntheticFtEncoding, SyntheticFtValues,
        build_synthetic_layer_stacks_with_ft_encoding, build_synthetic_layer_stacks_with_ft_values,
    };
    #[cfg(feature = "nnue-runtime-dimensions")]
    use crate::nnue::network::{
        LayerStackBucketMode, NNUENetwork, configure_layer_stack_routing,
        reset_layer_stack_progress_buckets, reset_layer_stack_progress_kpabs_weights,
    };
    #[cfg(feature = "nnue-runtime-dimensions")]
    use crate::position::{Position, SFEN_HIRATE};

    fn id(kind: NetTensorKind, bucket: Option<usize>, index: usize) -> NetCoefficientId {
        NetCoefficientId {
            kind,
            bucket,
            index,
        }
    }

    fn replace_combined_ft_payload(bytes: &[u8], payload: &[u8]) -> Vec<u8> {
        let layout = LayerStacksBinLayout::from_bytes(bytes).expect("layout");
        assert_eq!(layout.feature_transformer.encoding, FtBinEncoding::Leb128Combined);
        let original =
            layout.feature_transformer.biases.start..layout.feature_transformer.weights.end;
        let size_offset = original.start - 4;
        let size = u32::try_from(payload.len()).expect("payload size");
        let mut replaced = Vec::with_capacity(bytes.len() - original.len() + payload.len());
        replaced.extend_from_slice(&bytes[..size_offset]);
        replaced.extend_from_slice(&size.to_le_bytes());
        replaced.extend_from_slice(payload);
        replaced.extend_from_slice(&bytes[original.end..]);
        replaced
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
            let ft_biases = layout.decode_ft_biases(&synthetic.bytes).expect("FT biases");
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
                    layout
                        .coefficient_with_ft_biases(
                            &synthetic.bytes,
                            &coefficient,
                            Some(&ft_biases),
                        )
                        .expect("layout coefficient"),
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
                .coefficient(&synthetic.bytes, &id(NetTensorKind::FtBias, None, 0))
                .expect("original FT bias");
            synthetic.bytes[original.feature_transformer.weights.clone()].fill(0x80);

            let layout = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("layout");
            assert_eq!(layout.feature_transformer.weights, original.feature_transformer.weights);
            assert_eq!(
                layout
                    .coefficient(&synthetic.bytes, &id(NetTensorKind::FtBias, None, 0))
                    .expect("FT bias"),
                original_bias
            );
        }
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
        let original = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("original");
        let architecture = format!(
            "Features=HalfKP[{}->32x2],LayerStacks,l2=4,l3=3,PSQT=2,Threat=5,ThreatProfile=0",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS
        );
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&original.version.to_le_bytes());
        bytes.extend_from_slice(&original.network_hash.to_le_bytes());
        bytes.extend_from_slice(&(architecture.len() as u32).to_le_bytes());
        bytes.extend_from_slice(architecture.as_bytes());
        bytes.extend_from_slice(&(original.num_buckets as u32).to_le_bytes());
        bytes.extend_from_slice(
            &synthetic.bytes
                [original.feature_transformer.hash.start..original.buckets[0].fc_hash.start],
        );
        let psqt_bias_size = original.num_buckets * 4;
        let psqt_weight_size = original.ft_input_dimensions * original.num_buckets * 4;
        bytes.resize(bytes.len() + psqt_bias_size + psqt_weight_size, 0);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.resize(bytes.len() + 5 * original.l1, 0);
        bytes.extend_from_slice(&synthetic.bytes[original.buckets[0].fc_hash.start..]);

        let layout = LayerStacksBinLayout::from_bytes(&bytes).expect("extended layout");
        let psqt = layout.psqt.expect("PSQT");
        assert_eq!(psqt.biases.len(), psqt_bias_size);
        assert_eq!(psqt.weights.len(), psqt_weight_size);
        assert_eq!(layout.threat_profile.expect("profile").len(), 4);
        assert_eq!(layout.threat_weights.expect("threat").len(), 5 * original.l1);
    }

    #[test]
    fn leb128_blocks_decode_and_encode_to_identical_bytes() {
        for encoding in [
            SyntheticFtEncoding::Leb128Combined,
            SyntheticFtEncoding::Leb128Split,
        ] {
            let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
                "HalfKP",
                <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
                32,
                4,
                3,
                2,
                encoding,
            );
            let layout = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("layout");
            let values = decode_ft_tensor(&synthetic.bytes, &layout).expect("decode FT tensor");
            let encoded = replace_ft_block(&synthetic.bytes, &layout, &values).expect("encode");
            assert_eq!(encoded, synthetic.bytes, "{encoding:?}");
        }
    }

    #[test]
    fn combined_leb128_accepts_canonical_signed_i16_boundaries() {
        let synthetic = build_synthetic_layer_stacks_with_ft_values(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            2,
            2,
            1,
            2,
            SyntheticFtConfig {
                encoding: SyntheticFtEncoding::Leb128Combined,
                values: SyntheticFtValues::SignedBoundaries,
            },
        );
        let layout = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("layout");
        let values = decode_ft_tensor(&synthetic.bytes, &layout).expect("decode FT tensor");
        assert!(values.contains(&-1));
        assert!(values.contains(&64));
        assert!(values.contains(&-65));
        assert!(values.contains(&-8192));
        assert!(values.contains(&8191));
        assert!(values.contains(&i16::MIN));
        assert!(values.contains(&i16::MAX));
        let encoded = replace_ft_block(&synthetic.bytes, &layout, &values).expect("encode");
        assert_eq!(encoded, synthetic.bytes);
    }

    #[test]
    fn combined_leb128_rejects_non_canonical_and_out_of_i16_values() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            2,
            2,
            1,
            2,
            SyntheticFtEncoding::Leb128Combined,
        );
        let layout = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("layout");
        let original = &synthetic.bytes
            [layout.feature_transformer.biases.start..layout.feature_transformer.weights.end];

        let mut non_canonical_payload = vec![0x88, 0x00];
        non_canonical_payload.extend_from_slice(&original[1..]);
        let non_canonical = replace_combined_ft_payload(&synthetic.bytes, &non_canonical_payload);
        let error = apply_deltas_to_bytes(
            &non_canonical,
            &[NetDelta {
                id: id(NetTensorKind::OutputBias, Some(0), 0),
                delta: 1,
            }],
        )
        .expect_err("non-canonical payload");
        assert!(error.to_string().contains("not canonical"));

        for invalid_value in [i64::from(i16::MIN) - 1, i64::from(i16::MAX) + 1] {
            let mut invalid_prefix = Vec::new();
            encode_signed_leb128(invalid_value, &mut invalid_prefix);
            invalid_prefix.extend_from_slice(&original[1..]);
            let invalid = replace_combined_ft_payload(&synthetic.bytes, &invalid_prefix);
            let error = apply_deltas_to_bytes(
                &invalid,
                &[NetDelta {
                    id: id(NetTensorKind::OutputBias, Some(0), 0),
                    delta: 1,
                }],
            )
            .expect_err("out-of-range payload");
            assert!(error.to_string().contains("outside i16 range"));
        }
    }

    #[test]
    fn zero_deltas_preserve_every_byte_and_non_ft_regions_stay_verbatim() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Split,
        );
        let zero = NetDelta {
            id: id(NetTensorKind::FtBias, None, 0),
            delta: 0,
        };
        let (empty, empty_report) = apply_deltas_to_bytes(&synthetic.bytes, &[]).expect("empty");
        let (unchanged, zero_report) =
            apply_deltas_to_bytes(&synthetic.bytes, &[zero]).expect("zero");
        assert_eq!(empty, synthetic.bytes);
        assert_eq!(unchanged, synthetic.bytes);
        assert_eq!(empty_report.applied, 0);
        assert_eq!(zero_report.applied, 1);

        let layout = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("layout");
        let delta = NetDelta {
            id: id(NetTensorKind::FtBias, None, 0),
            delta: 64,
        };
        let (patched, _) = apply_deltas_to_bytes(&synthetic.bytes, &[delta]).expect("patch");
        let patched_layout = LayerStacksBinLayout::from_bytes(&patched).expect("patched layout");
        assert_eq!(
            &patched[..layout.feature_transformer.hash.end],
            &synthetic.bytes[..layout.feature_transformer.hash.end]
        );
        assert_eq!(patched_layout.version, layout.version);
        assert_eq!(patched_layout.architecture, layout.architecture);
        assert_eq!(
            patched_layout.feature_transformer.encoding,
            layout.feature_transformer.encoding
        );
        assert_eq!(
            &patched[patched_layout.feature_transformer.weights.start..],
            &synthetic.bytes[layout.feature_transformer.weights.start..]
        );
    }

    #[test]
    fn zero_ft_delta_mixed_with_output_delta_preserves_combined_ft_block() {
        let synthetic = build_synthetic_layer_stacks_with_ft_values(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            2,
            2,
            1,
            2,
            SyntheticFtConfig {
                encoding: SyntheticFtEncoding::Leb128Combined,
                values: SyntheticFtValues::SignedBoundaries,
            },
        );
        let input_layout =
            LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("input layout");
        let deltas = [
            NetDelta {
                id: id(NetTensorKind::FtBias, None, 0),
                delta: 0,
            },
            NetDelta {
                id: id(NetTensorKind::OutputBias, Some(0), 0),
                delta: 1,
            },
        ];
        let (patched, report) = apply_deltas_to_bytes(&synthetic.bytes, &deltas).expect("patch");
        let output_layout = LayerStacksBinLayout::from_bytes(&patched).expect("output layout");
        let input_ft =
            input_layout.feature_transformer.hash.end..input_layout.feature_transformer.weights.end;
        let output_ft = output_layout.feature_transformer.hash.end
            ..output_layout.feature_transformer.weights.end;
        assert_eq!(&patched[output_ft], &synthetic.bytes[input_ft]);
        assert_eq!(report.applied, 2);
        assert_eq!(report.clamped, 0);
    }

    #[test]
    fn byte_patching_uses_storage_type_saturation() {
        let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
            "HalfKP",
            <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
            32,
            4,
            3,
            2,
            SyntheticFtEncoding::Leb128Combined,
        );
        let deltas = [
            NetDelta {
                id: id(NetTensorKind::OutputWeight, Some(0), 0),
                delta: i32::MAX,
            },
            NetDelta {
                id: id(NetTensorKind::OutputBias, Some(0), 0),
                delta: i32::MAX,
            },
            NetDelta {
                id: id(NetTensorKind::FtBias, None, 0),
                delta: i32::MIN,
            },
        ];
        let (patched, report) = apply_deltas_to_bytes(&synthetic.bytes, &deltas).expect("patch");
        let layout = LayerStacksBinLayout::from_bytes(&patched).expect("layout");
        assert_eq!(report.applied, 3);
        assert_eq!(report.clamped, 3);
        assert_eq!(layout.coefficient(&patched, &deltas[0].id).expect("i8"), i8::MAX.into());
        assert_eq!(layout.coefficient(&patched, &deltas[1].id).expect("i32"), i32::MAX);
        assert_eq!(layout.coefficient(&patched, &deltas[2].id).expect("i16"), i16::MIN.into());
    }

    #[cfg(feature = "nnue-runtime-dimensions")]
    fn evaluate(bytes: &[u8], deltas: &[NetDelta], buckets: usize) -> i32 {
        let mut network = NNUENetwork::from_bytes(bytes).expect("network");
        network.apply_net_deltas(deltas).expect("deltas");
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbs, buckets, Some(buckets))
            .expect("routing");
        let mut position = Position::new();
        position.set_sfen(SFEN_HIRATE).expect("hirate");
        let mut evaluator = NNUEEvaluator::new_with_position(Arc::new(network), &position);
        evaluator.evaluate(&position).raw()
    }

    #[cfg(feature = "nnue-runtime-dimensions")]
    #[test]
    fn patched_net_evaluation_matches_runtime_deltas_for_both_leb128_forms() {
        reset_layer_stack_progress_kpabs_weights();
        for (buckets, encoding) in [
            (4, SyntheticFtEncoding::Leb128Combined),
            (9, SyntheticFtEncoding::Leb128Split),
        ] {
            let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
                "HalfKP",
                <HalfKPFeatureSet as FeatureSetTrait>::DIMENSIONS,
                32,
                4,
                2,
                buckets,
                encoding,
            );
            let selected_bucket = buckets / 2;
            let deltas = [
                NetDelta {
                    id: id(NetTensorKind::OutputWeight, Some(selected_bucket), 0),
                    delta: 64,
                },
                NetDelta {
                    id: id(NetTensorKind::OutputBias, Some(selected_bucket), 0),
                    delta: 256,
                },
                NetDelta {
                    id: id(NetTensorKind::FtBias, None, 0),
                    delta: 48,
                },
                NetDelta {
                    id: id(NetTensorKind::L2Weight, Some(selected_bucket), 3),
                    delta: 64,
                },
            ];
            let baseline = evaluate(&synthetic.bytes, &[], buckets);
            let runtime = evaluate(&synthetic.bytes, &deltas, buckets);
            let (patched, report) =
                apply_deltas_to_bytes(&synthetic.bytes, &deltas).expect("patch");
            let from_file = evaluate(&patched, &[], buckets);
            assert_ne!(runtime, baseline, "{encoding:?}");
            assert_eq!(from_file, runtime, "{encoding:?}");
            assert_eq!(report.applied, deltas.len());
            assert_eq!(report.clamped, 0);
        }
        reset_layer_stack_progress_buckets();
        reset_layer_stack_progress_kpabs_weights();
    }
}
