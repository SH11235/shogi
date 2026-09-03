//! 学習済み LayerStacks NNUE 係数への整数 delta。

use std::fmt;

/// net 係数を USI option として公開するときの接頭辞。
pub const NET_DELTA_OPTION_PREFIX: &str = "SPSA_NET_";

/// delta を適用できるテンソル種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NetTensorKind {
    /// bucket ごとの output 層 weight (`i8`)。
    OutputWeight,
    /// bucket ごとの output 層 bias (`i32`)。
    OutputBias,
    /// Feature Transformer bias (`i16`)。
    FtBias,
    /// bucket ごとの第 2 FC 層 weight (`i8`)。
    L2Weight,
}

impl NetTensorKind {
    /// USI option 名で使う token を返す。
    pub const fn token(self) -> &'static str {
        match self {
            Self::OutputWeight => "out_w",
            Self::OutputBias => "out_b",
            Self::FtBias => "ft_b",
            Self::L2Weight => "l2_w",
        }
    }
}

/// net 内の係数を一意に表す ID。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NetCoefficientId {
    /// テンソル種別。
    pub kind: NetTensorKind,
    /// bucket を持つテンソルの bucket index。
    pub bucket: Option<usize>,
    /// `.bin` ファイル格納順の flat index。
    pub index: usize,
}

impl NetCoefficientId {
    /// `SPSA_NET_*` USI option 名を parse する。
    pub fn parse_usi_name(name: &str) -> Option<Self> {
        let body = name.strip_prefix(NET_DELTA_OPTION_PREFIX)?;
        if let Some(index) = body.strip_prefix("ft_b_").and_then(parse_usize) {
            return Some(Self {
                kind: NetTensorKind::FtBias,
                bucket: None,
                index,
            });
        }

        for (prefix, kind) in [
            ("out_w_b", NetTensorKind::OutputWeight),
            ("out_b_b", NetTensorKind::OutputBias),
            ("l2_w_b", NetTensorKind::L2Weight),
        ] {
            let Some(indices) = body.strip_prefix(prefix) else {
                continue;
            };
            let (bucket, index) = indices.split_once('_')?;
            let bucket = parse_usize(bucket)?;
            let index = parse_usize(index)?;
            if kind == NetTensorKind::OutputBias && index != 0 {
                return None;
            }
            return Some(Self {
                kind,
                bucket: Some(bucket),
                index,
            });
        }
        None
    }

    /// canonical な `SPSA_NET_*` USI option 名を返す。
    pub fn usi_name(&self) -> String {
        match self.bucket {
            Some(bucket) => {
                format!("{NET_DELTA_OPTION_PREFIX}{}_b{bucket}_{}", self.kind.token(), self.index)
            }
            None => format!("{NET_DELTA_OPTION_PREFIX}{}_{}", self.kind.token(), self.index),
        }
    }
}

fn parse_usize(value: &str) -> Option<usize> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

/// 1 係数へ加える整数 delta。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetDelta {
    /// 対象係数。
    pub id: NetCoefficientId,
    /// 現在値へ加える値。
    pub delta: i32,
}

/// 1 種類のテンソル形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetTensorShape {
    /// bucket を持つ場合の bucket 数。FT bias は `None`。
    pub bucket_count: Option<usize>,
    /// 1 bucket あたりの要素数。FT bias は配列全体の要素数。
    pub element_count: usize,
}

impl NetTensorShape {
    #[cfg(any(test, feature = "nnue-runtime-dimensions", feature = "layerstack-arch"))]
    pub(crate) fn validate(self, id: &NetCoefficientId) -> Result<(), NetDeltaError> {
        let name = id.usi_name();
        match (self.bucket_count, id.bucket) {
            (Some(bucket_count), Some(bucket)) if bucket >= bucket_count => {
                return Err(NetDeltaError::BucketOutOfRange {
                    name,
                    bucket,
                    bucket_count,
                });
            }
            (Some(_), None) => return Err(NetDeltaError::MissingBucket { name }),
            (None, Some(_)) => return Err(NetDeltaError::UnexpectedBucket { name }),
            _ => {}
        }
        if id.index >= self.element_count {
            return Err(NetDeltaError::IndexOutOfRange {
                name,
                index: id.index,
                element_count: self.element_count,
            });
        }
        Ok(())
    }
}

/// net delta 適用結果。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NetDeltaReport {
    /// 適用した delta 数。
    pub applied: usize,
    /// 要素型の範囲へ clamp された delta 数。
    pub clamped: usize,
}

/// net delta の検証・適用エラー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetDeltaError {
    /// LayerStacks 以外の architecture が指定された。
    UnsupportedArchitecture {
        /// 読み込まれている architecture 名。
        architecture: String,
    },
    /// bucket 必須の kind に bucket が無い。
    MissingBucket {
        /// 問題の係数名。
        name: String,
    },
    /// bucket を持たない kind に bucket が指定された。
    UnexpectedBucket {
        /// 問題の係数名。
        name: String,
    },
    /// bucket index が範囲外。
    BucketOutOfRange {
        /// 問題の係数名。
        name: String,
        /// 指定された bucket index。
        bucket: usize,
        /// 読み込まれた net の bucket 数。
        bucket_count: usize,
    },
    /// flat index が範囲外。
    IndexOutOfRange {
        /// 問題の係数名。
        name: String,
        /// 指定された flat index。
        index: usize,
        /// 対象テンソルの要素数。
        element_count: usize,
    },
}

impl fmt::Display for NetDeltaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedArchitecture { architecture } => {
                write!(formatter, "unsupported architecture \"{architecture}\"")
            }
            Self::MissingBucket { name } => write!(formatter, "{name}: bucket is required"),
            Self::UnexpectedBucket { name } => {
                write!(formatter, "{name}: bucket is not allowed")
            }
            Self::BucketOutOfRange {
                name,
                bucket,
                bucket_count,
            } => write!(
                formatter,
                "{name}: bucket {bucket} is out of range (bucket count: {bucket_count})"
            ),
            Self::IndexOutOfRange {
                name,
                index,
                element_count,
            } => write!(
                formatter,
                "{name}: index {index} is out of range (element count: {element_count})"
            ),
        }
    }
}

impl std::error::Error for NetDeltaError {}

#[cfg(any(test, feature = "nnue-runtime-dimensions", feature = "layerstack-arch"))]
pub(crate) fn add_i8_delta(value: i8, delta: i32) -> (i8, bool) {
    let sum = i64::from(value) + i64::from(delta);
    let clamped = sum.clamp(i64::from(i8::MIN), i64::from(i8::MAX));
    (clamped as i8, sum != clamped)
}

#[cfg(any(test, feature = "nnue-runtime-dimensions", feature = "layerstack-arch"))]
pub(crate) fn add_i16_delta(value: i16, delta: i32) -> (i16, bool) {
    let sum = i64::from(value) + i64::from(delta);
    let clamped = sum.clamp(i64::from(i16::MIN), i64::from(i16::MAX));
    (clamped as i16, sum != clamped)
}

#[cfg(any(test, feature = "nnue-runtime-dimensions", feature = "layerstack-arch"))]
pub(crate) fn add_i32_delta(value: i32, delta: i32) -> (i32, bool) {
    let sum = i64::from(value) + i64::from(delta);
    let clamped = sum.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
    (clamped as i32, sum != clamped)
}

#[cfg(all(
    test,
    any(feature = "nnue-runtime-dimensions", feature = "layerstack-arch")
))]
pub(crate) mod test_utils {
    use crate::nnue::constants::NNUE_VERSION_LAYERSTACK_NUM_BUCKETS;
    use crate::nnue::layers::padded_input;
    use crate::nnue::leb128::LEB128_MAGIC;

    pub(crate) struct SyntheticBucketOffsets {
        pub(crate) l2_weights: usize,
        pub(crate) output_bias: usize,
        pub(crate) output_weights: usize,
    }

    pub(crate) struct SyntheticLayerStacksBin {
        pub(crate) bytes: Vec<u8>,
        pub(crate) ft_biases: usize,
        pub(crate) buckets: Vec<SyntheticBucketOffsets>,
    }

    struct Lcg(u32);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            self.0
        }

        fn range(&mut self, min: i32, max: i32) -> i32 {
            min + (self.next() % ((max - min + 1) as u32)) as i32
        }
    }

    pub(crate) fn encode_single_byte_signed_leb128(value: i32) -> u8 {
        assert!((-64..=63).contains(&value));
        (value as u8) & 0x7f
    }

    fn append_affine(
        bytes: &mut Vec<u8>,
        input_dim: usize,
        output_dim: usize,
        rng: &mut Lcg,
        bias_range: std::ops::RangeInclusive<i32>,
        weight_max: i32,
    ) -> usize {
        for _ in 0..output_dim {
            let bias = rng.range(*bias_range.start(), *bias_range.end());
            bytes.extend_from_slice(&bias.to_le_bytes());
        }
        let weights = bytes.len();
        for _ in 0..output_dim * padded_input(input_dim) {
            bytes.push(rng.range(1, weight_max) as u8);
        }
        weights
    }

    pub(crate) fn build_synthetic_layer_stacks(
        feature_name: &str,
        input_dimensions: usize,
        l1: usize,
        l2: usize,
        l3: usize,
        num_buckets: usize,
    ) -> SyntheticLayerStacksBin {
        let arch = format!(
            "Features={feature_name}[{input_dimensions}->{l1}x2],LayerStacks,l2={l2},l3={l3}"
        );
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&NNUE_VERSION_LAYERSTACK_NUM_BUCKETS.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(arch.len() as u32).to_le_bytes());
        bytes.extend_from_slice(arch.as_bytes());
        bytes.extend_from_slice(&(num_buckets as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());

        let mut rng = Lcg(0x5eed_1234);
        let ft_element_count = l1 + input_dimensions * l1;
        bytes.extend_from_slice(LEB128_MAGIC);
        bytes.extend_from_slice(&(ft_element_count as u32).to_le_bytes());
        let ft_biases = bytes.len();
        for _ in 0..l1 {
            bytes.push(encode_single_byte_signed_leb128(rng.range(8, 15)));
        }
        for _ in l1..ft_element_count {
            bytes.push(encode_single_byte_signed_leb128(rng.range(1, 2)));
        }

        let mut buckets = Vec::with_capacity(num_buckets);
        for _ in 0..num_buckets {
            bytes.extend_from_slice(&0u32.to_le_bytes());
            append_affine(&mut bytes, l1, l2, &mut rng, 64..=127, 2);
            let l2_weights = append_affine(&mut bytes, 2 * (l2 - 1), l3, &mut rng, 128..=255, 2);
            let output_bias = bytes.len();
            let output_weights = append_affine(&mut bytes, l3, 1, &mut rng, 128..=255, 8);
            buckets.push(SyntheticBucketOffsets {
                l2_weights,
                output_bias,
                output_weights,
            });
        }
        SyntheticLayerStacksBin {
            bytes,
            ft_biases,
            buckets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_deltas_clamp_at_storage_bounds() {
        assert_eq!(add_i8_delta(126, 1), (127, false));
        assert_eq!(add_i8_delta(127, 1), (127, true));
        assert_eq!(add_i16_delta(i16::MIN + 1, -1), (i16::MIN, false));
        assert_eq!(add_i16_delta(i16::MIN, -1), (i16::MIN, true));
        assert_eq!(add_i32_delta(i32::MAX - 1, 1), (i32::MAX, false));
        assert_eq!(add_i32_delta(i32::MAX, 1), (i32::MAX, true));
    }

    #[test]
    fn tensor_shape_validates_bucket_and_index() {
        let shape = NetTensorShape {
            bucket_count: Some(2),
            element_count: 3,
        };
        let valid = NetCoefficientId {
            kind: NetTensorKind::OutputWeight,
            bucket: Some(1),
            index: 2,
        };
        assert_eq!(shape.validate(&valid), Ok(()));

        let mut invalid = valid.clone();
        invalid.bucket = None;
        assert!(matches!(shape.validate(&invalid), Err(NetDeltaError::MissingBucket { .. })));
        invalid.bucket = Some(2);
        assert!(matches!(shape.validate(&invalid), Err(NetDeltaError::BucketOutOfRange { .. })));
        invalid.bucket = Some(1);
        invalid.index = 3;
        assert!(matches!(shape.validate(&invalid), Err(NetDeltaError::IndexOutOfRange { .. })));
    }

    #[test]
    fn usi_name_round_trip() {
        for name in [
            "SPSA_NET_out_w_b3_17",
            "SPSA_NET_out_b_b0_0",
            "SPSA_NET_ft_b_1023",
            "SPSA_NET_l2_w_b2_45",
        ] {
            let id = NetCoefficientId::parse_usi_name(name).expect("valid name");
            assert_eq!(id.usi_name(), name);
        }
    }

    #[test]
    fn usi_name_rejects_invalid_forms() {
        for name in [
            "SPSA_out_w_b3_17",
            "SPSA_NET_out_w_17",
            "SPSA_NET_ft_b_b0_17",
            "SPSA_NET_ft_b_",
            "SPSA_NET_ft_b_-1",
            "SPSA_NET_out_w_bx_17",
            "SPSA_NET_out_w_b3_x",
            "SPSA_NET_out_w_b3_17_extra",
            "SPSA_NET_out_b_b0_1",
            "SPSA_NET_unknown_b0_0",
        ] {
            assert!(NetCoefficientId::parse_usi_name(name).is_none(), "{name}");
        }
    }
}
