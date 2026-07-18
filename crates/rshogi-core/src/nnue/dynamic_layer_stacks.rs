//! Runtime-dimension LayerStacks inference for `edition-universal`.

use std::io::{self, Read, Seek, SeekFrom};

use super::accumulator::{
    AlignedBox, DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES,
};
use super::bona_piece_effect_bucket::EffectBucketConfig;
use super::constants::{
    DEFAULT_NUM_BUCKETS, FV_SCALE_HALFKA, MAX_ARCH_LEN, MAX_LAYER_STACK_BUCKETS,
    NNUE_VERSION_HALFKA, NNUE_VERSION_LAYERSTACK_NUM_BUCKETS,
};
use super::dynamic_halfkx::{DynamicAffine, validate_dimension};
use super::effect_bucket_features::append_active_effect_bucket;
use super::features::{
    FeatureSet as FeatureSetTrait, HalfKPFeatureSet, HalfKaHmMergedFeatureSet,
    HalfKaHmSplitFeatureSet, HalfKaMergedFeatureSet, HalfKaSplitFeatureSet,
};
use super::layers::padded_input;
use super::leb128::read_compressed_tensor_i16_all;
use super::network::{
    LayerStackBucketMode, compute_layer_stack_progress8kpabs_bucket_index, get_fv_scale_override,
    get_layer_stack_bucket_mode, get_layer_stack_progress_kpabs_weights, parse_fv_scale_from_arch,
};
use super::network_layer_stacks::compute_layer_stack_kingrank9_bucket_index;
use super::spec::{
    Activation, ArchitectureSpec, FeatureSet, parse_arch_dimensions,
    parse_feature_input_dimensions, parse_feature_set_from_arch,
};
use crate::position::Position;
use crate::types::{Color, Value};

const STACK_CAPACITY: usize = 256;

#[derive(Clone, Copy)]
enum RuntimeLsFeature {
    HalfKP,
    HalfKaHmMerged,
    HalfKaSplit,
    HalfKaMerged,
    HalfKaHmSplit,
    EffectBucket(EffectBucketConfig),
}

impl RuntimeLsFeature {
    fn dimensions(self) -> usize {
        match self {
            Self::HalfKP => HalfKPFeatureSet::DIMENSIONS,
            Self::HalfKaHmMerged => HalfKaHmMergedFeatureSet::DIMENSIONS,
            Self::HalfKaSplit => HalfKaSplitFeatureSet::DIMENSIONS,
            Self::HalfKaMerged => HalfKaMergedFeatureSet::DIMENSIONS,
            Self::HalfKaHmSplit => HalfKaHmSplitFeatureSet::DIMENSIONS,
            Self::EffectBucket(config) => config.dimensions(),
        }
    }

    fn active(self, pos: &Position, perspective: Color) -> IndexList<MAX_ACTIVE_FEATURES> {
        match self {
            Self::HalfKP => HalfKPFeatureSet::collect_active_indices(pos, perspective),
            Self::HalfKaHmMerged => {
                HalfKaHmMergedFeatureSet::collect_active_indices(pos, perspective)
            }
            Self::HalfKaSplit => HalfKaSplitFeatureSet::collect_active_indices(pos, perspective),
            Self::HalfKaMerged => HalfKaMergedFeatureSet::collect_active_indices(pos, perspective),
            Self::HalfKaHmSplit => {
                HalfKaHmSplitFeatureSet::collect_active_indices(pos, perspective)
            }
            Self::EffectBucket(config) => {
                let mut active = IndexList::new();
                append_active_effect_bucket(pos, config, perspective, &mut active);
                active
            }
        }
    }

    fn changed(
        self,
        dirty: &DirtyPiece,
        perspective: Color,
        pos: &Position,
    ) -> Option<(IndexList<MAX_CHANGED_FEATURES>, IndexList<MAX_CHANGED_FEATURES>)> {
        let king_sq = pos.king_square(perspective);
        match self {
            Self::HalfKP => {
                Some(HalfKPFeatureSet::collect_changed_indices(dirty, perspective, king_sq))
            }
            Self::HalfKaHmMerged => {
                Some(HalfKaHmMergedFeatureSet::collect_changed_indices(dirty, perspective, king_sq))
            }
            Self::HalfKaSplit => {
                Some(HalfKaSplitFeatureSet::collect_changed_indices(dirty, perspective, king_sq))
            }
            Self::HalfKaMerged => {
                Some(HalfKaMergedFeatureSet::collect_changed_indices(dirty, perspective, king_sq))
            }
            Self::HalfKaHmSplit => {
                Some(HalfKaHmSplitFeatureSet::collect_changed_indices(dirty, perspective, king_sq))
            }
            Self::EffectBucket(_) => None,
        }
    }

    fn needs_refresh(self, dirty: &DirtyPiece, perspective: Color) -> bool {
        match self {
            Self::HalfKP => HalfKPFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaHmMerged => HalfKaHmMergedFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaSplit => HalfKaSplitFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaMerged => HalfKaMergedFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaHmSplit => HalfKaHmSplitFeatureSet::needs_refresh(dirty, perspective),
            Self::EffectBucket(_) => true,
        }
    }
}

struct DynamicLsBucket {
    l1: DynamicAffine,
    l2: DynamicAffine,
    output: DynamicAffine,
}

/// Runtime-dimension LayerStacks network.
pub struct DynamicLayerStacksNetwork {
    spec: ArchitectureSpec,
    feature: RuntimeLsFeature,
    input_dimensions: usize,
    num_buckets: usize,
    ft_biases: AlignedBox<i16>,
    ft_weights: AlignedBox<i16>,
    psqt_biases: AlignedBox<i32>,
    psqt_weights: AlignedBox<i32>,
    threat_dimensions: usize,
    threat_weights: AlignedBox<i8>,
    buckets: Vec<DynamicLsBucket>,
    fv_scale: i32,
}

impl DynamicLayerStacksNetwork {
    pub(crate) fn read<R: Read + Seek>(
        reader: &mut R,
        psqt_override: Option<bool>,
    ) -> io::Result<Self> {
        let file_size = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;
        let mut buf4 = [0; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);
        if version != NNUE_VERSION_HALFKA && version != NNUE_VERSION_LAYERSTACK_NUM_BUCKETS {
            return Err(invalid("invalid LayerStacks NNUE version"));
        }
        reader.read_exact(&mut buf4)?;
        reader.read_exact(&mut buf4)?;
        let arch_len = u32::from_le_bytes(buf4) as usize;
        if arch_len == 0 || arch_len > MAX_ARCH_LEN {
            return Err(invalid("invalid architecture string length"));
        }
        let mut arch = vec![0; arch_len];
        reader.read_exact(&mut arch)?;
        let arch =
            std::str::from_utf8(&arch).map_err(|_| invalid("architecture string is not UTF-8"))?;
        let num_buckets = if version == NNUE_VERSION_LAYERSTACK_NUM_BUCKETS {
            reader.read_exact(&mut buf4)?;
            u32::from_le_bytes(buf4) as usize
        } else {
            DEFAULT_NUM_BUCKETS
        };
        if !(1..=MAX_LAYER_STACK_BUCKETS).contains(&num_buckets) {
            return Err(invalid(format!(
                "invalid LayerStacks num_buckets={num_buckets}; expected 1..={MAX_LAYER_STACK_BUCKETS}"
            )));
        }

        let (l1, l2, l3) = parse_arch_dimensions(arch);
        if l1 == 0 || l1 % 2 != 0 || l2 < 2 || l3 == 0 {
            return Err(invalid("invalid LayerStacks dimensions"));
        }
        validate_dimension("LayerStacks l1", l1)?;
        validate_dimension("LayerStacks l2", l2)?;
        validate_dimension("LayerStacks l3", l3)?;
        let reported_input_dimensions = parse_feature_input_dimensions(arch)
            .ok_or_else(|| invalid("missing FT input dimensions"))?;
        let threat_dimensions = parse_token_usize(arch, "Threat=").unwrap_or(0);
        if arch.contains("Threat=") && threat_dimensions == 0 {
            return Err(invalid("malformed Threat token"));
        }
        #[cfg(feature = "nnue-threat")]
        if threat_dimensions != 0 && threat_dimensions != super::threat_features::THREAT_DIMENSIONS
        {
            return Err(invalid(format!(
                "Threat dimensions mismatch: model={threat_dimensions}, runtime={}",
                super::threat_features::THREAT_DIMENSIONS
            )));
        }
        #[cfg(not(feature = "nnue-threat"))]
        if threat_dimensions != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Threat model requires nnue-threat",
            ));
        }
        let input_dimensions = reported_input_dimensions
            .checked_sub(threat_dimensions)
            .ok_or_else(|| invalid("Threat dimensions exceed reported FT input dimensions"))?;
        let parsed_feature = detect_feature(arch)?;
        let feature = match parsed_feature {
            FeatureSet::HalfKP => RuntimeLsFeature::HalfKP,
            FeatureSet::HalfKaHmMerged => RuntimeLsFeature::HalfKaHmMerged,
            FeatureSet::HalfKaSplit => RuntimeLsFeature::HalfKaSplit,
            FeatureSet::HalfKaMerged => RuntimeLsFeature::HalfKaMerged,
            FeatureSet::HalfKaHmSplit => RuntimeLsFeature::HalfKaHmSplit,
            FeatureSet::HalfKaHmMergedEffectBucket => {
                let config = parse_effect_config(arch)
                    .ok_or_else(|| invalid("malformed EffectBucket token"))?;
                RuntimeLsFeature::EffectBucket(config)
            }
            FeatureSet::LayerStacks => {
                return Err(invalid("LayerStacks header does not identify its FT feature set"));
            }
        };
        if input_dimensions != feature.dimensions() {
            return Err(invalid(format!(
                "FT input dimension mismatch: header={input_dimensions}, runtime={}",
                feature.dimensions()
            )));
        }

        reader.read_exact(&mut buf4)?;
        let first = read_compressed_tensor_i16_all(reader)?;
        let weight_len = input_dimensions
            .checked_mul(l1)
            .ok_or_else(|| invalid("FT dimensions overflow"))?;
        let (bias_vec, weight_vec) = if first.len() == l1 + weight_len {
            (first[..l1].to_vec(), first[l1..].to_vec())
        } else if first.len() == l1 {
            let weights = read_compressed_tensor_i16_all(reader)?;
            if weights.len() != weight_len {
                return Err(invalid("FT weight block size mismatch"));
            }
            (first, weights)
        } else {
            return Err(invalid("FT LEB128 block size mismatch"));
        };
        let mut ft_biases = AlignedBox::new_zeroed(l1);
        ft_biases.copy_from_slice(&bias_vec);
        let mut ft_weights = AlignedBox::new_zeroed(weight_len);
        ft_weights.copy_from_slice(&weight_vec);

        let has_psqt = psqt_override.unwrap_or_else(|| arch.contains("PSQT="));
        let mut psqt_biases = AlignedBox::new_zeroed(if has_psqt { num_buckets } else { 0 });
        let mut psqt_weights = AlignedBox::new_zeroed(if has_psqt {
            input_dimensions * num_buckets
        } else {
            0
        });
        if has_psqt {
            read_i32s(reader, &mut psqt_biases)?;
            read_i32s(reader, &mut psqt_weights)?;
        }

        if arch.contains("ThreatProfile=") {
            reader.read_exact(&mut buf4)?;
            if u32::from_le_bytes(buf4) != 0 {
                return Err(invalid(
                    "runtime threat profiles other than profile 0 are not yet supported",
                ));
            }
        }
        let mut threat_weights: AlignedBox<i8> = AlignedBox::new_zeroed(
            threat_dimensions
                .checked_mul(l1)
                .ok_or_else(|| invalid("Threat dimensions overflow"))?,
        );
        if !threat_weights.is_empty() {
            let bytes = unsafe {
                std::slice::from_raw_parts_mut(
                    threat_weights.as_mut_ptr().cast::<u8>(),
                    threat_weights.len(),
                )
            };
            reader.read_exact(bytes)?;
        }

        let mut buckets = Vec::with_capacity(num_buckets);
        for _ in 0..num_buckets {
            reader.read_exact(&mut buf4)?; // per-bucket FC hash
            buckets.push(DynamicLsBucket {
                l1: DynamicAffine::read(reader, l1, l2)?,
                l2: DynamicAffine::read(reader, 2 * (l2 - 1), l3)?,
                output: DynamicAffine::read(reader, l3, 1)?,
            });
        }
        let consumed = reader.stream_position()?;
        if consumed != file_size {
            return Err(invalid(format!(
                "unexpected trailing LayerStacks data: consumed={consumed}, file_size={file_size}"
            )));
        }
        Ok(Self {
            // Keep public introspection compatible with the const-generic LayerStacks path;
            // the concrete FT remains in `feature` for runtime dispatch.
            spec: ArchitectureSpec::new(FeatureSet::LayerStacks, l1, l2, l3, Activation::CReLU),
            feature,
            input_dimensions,
            num_buckets,
            ft_biases,
            ft_weights,
            psqt_biases,
            psqt_weights,
            threat_dimensions,
            threat_weights,
            buckets,
            fv_scale: parse_fv_scale_from_arch(arch).unwrap_or(FV_SCALE_HALFKA),
        })
    }

    pub(crate) fn spec(&self) -> ArchitectureSpec {
        self.spec
    }
    pub(crate) fn num_buckets(&self) -> usize {
        self.num_buckets
    }
    pub(crate) fn requires_board_effects(&self) -> bool {
        matches!(self.feature, RuntimeLsFeature::EffectBucket(_))
    }
    pub(crate) fn new_stack(&self) -> DynamicLayerStacksStack {
        DynamicLayerStacksStack::new(self)
    }

    pub(crate) fn refresh(&self, pos: &Position, stack: &mut DynamicLayerStacksStack) {
        let current = stack.current;
        for perspective in [Color::Black, Color::White] {
            self.refresh_perspective(pos, perspective, stack);
        }
        stack.computed[current] = true;
    }

    fn refresh_perspective(
        &self,
        pos: &Position,
        perspective: Color,
        stack: &mut DynamicLayerStacksStack,
    ) {
        let current = stack.current;
        let p = perspective.index();
        let start = (current * 2 + p) * self.spec.l1;
        let acc = &mut stack.accumulations[start..start + self.spec.l1];
        acc.copy_from_slice(&self.ft_biases);
        let psqt_start = (current * 2 + p) * self.num_buckets;
        let psqt = &mut stack.psqt[psqt_start..psqt_start + self.num_buckets];
        if self.psqt_biases.is_empty() {
            psqt.fill(0);
        } else {
            psqt.copy_from_slice(&self.psqt_biases);
        }
        for index in self.feature.active(pos, perspective).iter() {
            add_i16(acc, self.ft_row(index));
            if !self.psqt_weights.is_empty() {
                add_i32(
                    psqt,
                    &self.psqt_weights[index * self.num_buckets..(index + 1) * self.num_buckets],
                );
            }
        }
        self.refresh_threat(pos, perspective, stack);
    }

    fn refresh_threat(
        &self,
        pos: &Position,
        perspective: Color,
        stack: &mut DynamicLayerStacksStack,
    ) {
        let start = (stack.current * 2 + perspective.index()) * self.spec.l1;
        let threat = &mut stack.threat_accumulations[start..start + self.spec.l1];
        threat.fill(0);
        if self.threat_dimensions != 0 {
            #[cfg(feature = "nnue-threat")]
            super::threat_features::for_each_active_threat_index(
                pos,
                perspective,
                pos.king_square(perspective),
                |idx| {
                    let row = &self.threat_weights[idx * self.spec.l1..(idx + 1) * self.spec.l1];
                    for (a, &w) in threat.iter_mut().zip(row) {
                        *a = a.wrapping_add(i16::from(w));
                    }
                },
            );
        }
    }

    pub(crate) fn ensure(&self, pos: &Position, stack: &mut DynamicLayerStacksStack) {
        if stack.computed[stack.current] {
            return;
        }
        if stack.current == 0 || !stack.computed[stack.current - 1] {
            self.refresh(pos, stack);
            return;
        }
        let current = stack.current;
        let dirty = stack.dirty[current];
        for perspective in [Color::Black, Color::White] {
            if self.feature.needs_refresh(&dirty, perspective) {
                self.refresh_perspective(pos, perspective, stack);
                continue;
            }
            let Some((removed, added)) = self.feature.changed(&dirty, perspective, pos) else {
                self.refresh_perspective(pos, perspective, stack);
                continue;
            };
            let p = perspective.index();
            let l1 = self.spec.l1;
            let prev_start = ((current - 1) * 2 + p) * l1;
            let curr_start = (current * 2 + p) * l1;
            let (before, after) = stack.accumulations.split_at_mut(curr_start);
            let prev = &before[prev_start..prev_start + l1];
            let curr = &mut after[..l1];
            curr.copy_from_slice(prev);
            for index in removed.iter() {
                sub_i16(curr, self.ft_row(index));
            }
            for index in added.iter() {
                add_i16(curr, self.ft_row(index));
            }

            let prev_psqt_start = ((current - 1) * 2 + p) * self.num_buckets;
            let curr_psqt_start = (current * 2 + p) * self.num_buckets;
            let (before, after) = stack.psqt.split_at_mut(curr_psqt_start);
            let prev = &before[prev_psqt_start..prev_psqt_start + self.num_buckets];
            let curr = &mut after[..self.num_buckets];
            curr.copy_from_slice(prev);
            if !self.psqt_weights.is_empty() {
                for index in removed.iter() {
                    sub_i32(
                        curr,
                        &self.psqt_weights
                            [index * self.num_buckets..(index + 1) * self.num_buckets],
                    );
                }
                for index in added.iter() {
                    add_i32(
                        curr,
                        &self.psqt_weights
                            [index * self.num_buckets..(index + 1) * self.num_buckets],
                    );
                }
            }
            self.refresh_threat(pos, perspective, stack);
        }
        stack.computed[current] = true;
    }

    pub(crate) fn evaluate(&self, pos: &Position, stack: &mut DynamicLayerStacksStack) -> Value {
        let l1 = self.spec.l1;
        let base = stack.current * 2 * l1;
        let us = &stack.accumulations
            [base + pos.side_to_move().index() * l1..base + (pos.side_to_move().index() + 1) * l1];
        let them = &stack.accumulations[base + (!pos.side_to_move()).index() * l1
            ..base + ((!pos.side_to_move()).index() + 1) * l1];
        let us_threat = &stack.threat_accumulations
            [base + pos.side_to_move().index() * l1..base + (pos.side_to_move().index() + 1) * l1];
        let them_threat = &stack.threat_accumulations[base + (!pos.side_to_move()).index() * l1
            ..base + ((!pos.side_to_move()).index() + 1) * l1];
        sqr_transform_with_threat(us, us_threat, them, them_threat, &mut stack.transformed[..l1]);
        let bucket_index = match get_layer_stack_bucket_mode() {
            LayerStackBucketMode::KingRank9 => compute_layer_stack_kingrank9_bucket_index(
                pos,
                pos.side_to_move(),
                self.num_buckets,
            ),
            LayerStackBucketMode::Progress8KPAbs => {
                compute_layer_stack_progress8kpabs_bucket_index(
                    pos,
                    pos.side_to_move(),
                    get_layer_stack_progress_kpabs_weights(),
                    self.num_buckets,
                )
            }
        };
        let bucket = &self.buckets[bucket_index];
        bucket.l1.propagate(&stack.transformed, &mut stack.l1_out);
        let main_dim = self.spec.l2 - 1;
        let skip = stack.l1_out[main_dim];
        stack.l2_input.fill(0);
        for i in 0..main_dim {
            let v = stack.l1_out[i];
            stack.l2_input[i] = (((i64::from(v) * i64::from(v)) >> 19).clamp(0, 127)) as u8;
            stack.l2_input[main_dim + i] = (v >> 6).clamp(0, 127) as u8;
        }
        bucket.l2.propagate(&stack.l2_input, &mut stack.l2_out);
        stack.l3_input.fill(0);
        for (dst, &v) in stack.l3_input.iter_mut().zip(stack.l2_out.iter()) {
            *dst = (v >> 6).clamp(0, 127) as u8;
        }
        let mut output = [0];
        bucket.output.propagate(&stack.l3_input, &mut output);
        let psqt_base = stack.current * 2 * self.num_buckets;
        let psqt = if self.psqt_weights.is_empty() {
            0
        } else {
            (stack.psqt[psqt_base + pos.side_to_move().index() * self.num_buckets + bucket_index]
                - stack.psqt
                    [psqt_base + (!pos.side_to_move()).index() * self.num_buckets + bucket_index])
                / 2
        };
        Value::new(
            (output[0] + skip).saturating_add(psqt)
                / get_fv_scale_override().unwrap_or(self.fv_scale),
        )
    }

    fn ft_row(&self, index: usize) -> &[i16] {
        debug_assert!(index < self.input_dimensions);
        &self.ft_weights[index * self.spec.l1..(index + 1) * self.spec.l1]
    }
}

pub struct DynamicLayerStacksStack {
    current: usize,
    accumulations: AlignedBox<i16>,
    threat_accumulations: AlignedBox<i16>,
    psqt: AlignedBox<i32>,
    computed: Vec<bool>,
    dirty: Vec<DirtyPiece>,
    transformed: AlignedBox<u8>,
    l1_out: AlignedBox<i32>,
    l2_input: AlignedBox<u8>,
    l2_out: AlignedBox<i32>,
    l3_input: AlignedBox<u8>,
}

impl DynamicLayerStacksStack {
    fn new(net: &DynamicLayerStacksNetwork) -> Self {
        Self {
            current: 0,
            accumulations: AlignedBox::new_zeroed(STACK_CAPACITY * 2 * net.spec.l1),
            threat_accumulations: AlignedBox::new_zeroed(STACK_CAPACITY * 2 * net.spec.l1),
            psqt: AlignedBox::new_zeroed(STACK_CAPACITY * 2 * net.num_buckets),
            computed: vec![false; STACK_CAPACITY],
            dirty: (0..STACK_CAPACITY).map(|_| DirtyPiece::default()).collect(),
            transformed: AlignedBox::new_zeroed(padded_input(net.spec.l1)),
            l1_out: AlignedBox::new_zeroed(net.spec.l2),
            l2_input: AlignedBox::new_zeroed(padded_input(2 * (net.spec.l2 - 1))),
            l2_out: AlignedBox::new_zeroed(net.spec.l3),
            l3_input: AlignedBox::new_zeroed(padded_input(net.spec.l3)),
        }
    }
    pub(crate) fn reset(&mut self) {
        self.current = 0;
        self.computed[0] = false;
        self.dirty[0].clear();
    }
    pub(crate) fn push(&mut self, dirty: DirtyPiece) {
        assert!(self.current + 1 < STACK_CAPACITY, "dynamic NNUE accumulator stack overflow");
        self.current += 1;
        self.computed[self.current] = false;
        self.dirty[self.current] = dirty;
    }
    pub(crate) fn pop(&mut self) {
        if self.current > 0 {
            self.current -= 1;
        }
    }
}

fn detect_feature(arch: &str) -> io::Result<FeatureSet> {
    if arch.contains("EffectBucket=") || arch.contains("E4=") {
        return Ok(FeatureSet::HalfKaHmMergedEffectBucket);
    }
    if let Ok(feature) = parse_feature_set_from_arch(arch)
        && feature != FeatureSet::LayerStacks
    {
        return Ok(feature);
    }
    for (token, feature) in [
        ("HalfKP", FeatureSet::HalfKP),
        ("HalfKaSplit", FeatureSet::HalfKaSplit),
        ("HalfKaMerged", FeatureSet::HalfKaMerged),
        ("HalfKaHmSplit", FeatureSet::HalfKaHmSplit),
        ("HalfKaHmMerged", FeatureSet::HalfKaHmMerged),
    ] {
        if arch.contains(token) {
            return Ok(feature);
        }
    }
    Err(invalid("unknown LayerStacks FT"))
}
fn parse_effect_config(arch: &str) -> Option<EffectBucketConfig> {
    let token = arch
        .split(',')
        .find_map(|p| p.strip_prefix("EffectBucket=").or_else(|| p.strip_prefix("E4=")))?;
    match token {
        "2x2fixed" => Some(EffectBucketConfig::KINGFIXED_2X2),
        "2x2bucketed" => Some(EffectBucketConfig::KINGBUCKETED_2X2),
        "3x3fixed" => Some(EffectBucketConfig::KINGFIXED_3X3),
        "3x3bucketed" => Some(EffectBucketConfig::KINGBUCKETED_3X3),
        "4xfixed" => Some(EffectBucketConfig::KINGFIXED_2X2),
        "4xbucketed" => Some(EffectBucketConfig::KINGBUCKETED_2X2),
        "9xfixed" => Some(EffectBucketConfig::KINGFIXED_3X3),
        "9xbucketed" => Some(EffectBucketConfig::KINGBUCKETED_3X3),
        _ => None,
    }
}
fn parse_token_usize(arch: &str, token: &str) -> Option<usize> {
    arch.split(',').find_map(|p| p.strip_prefix(token))?.parse().ok()
}
fn read_i32s<R: Read>(reader: &mut R, dst: &mut [i32]) -> io::Result<()> {
    let mut b = [0; 4];
    for v in dst {
        reader.read_exact(&mut b)?;
        *v = i32::from_le_bytes(b);
    }
    Ok(())
}
fn add_i16(a: &mut [i16], b: &[i16]) {
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    unsafe {
        use std::arch::wasm32::*;
        let chunks = a.len().min(b.len()) / 8;
        for i in 0..chunks {
            let offset = i * 8;
            let va = v128_load(a.as_ptr().add(offset).cast::<v128>());
            let vb = v128_load(b.as_ptr().add(offset).cast::<v128>());
            v128_store(a.as_mut_ptr().add(offset).cast::<v128>(), i16x8_add(va, vb));
        }
        for (a, &b) in a[chunks * 8..].iter_mut().zip(&b[chunks * 8..]) {
            *a = a.wrapping_add(b);
        }
        return;
    }
    #[allow(unreachable_code)]
    for (a, &b) in a.iter_mut().zip(b) {
        *a = a.wrapping_add(b);
    }
}
fn sub_i16(a: &mut [i16], b: &[i16]) {
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    unsafe {
        use std::arch::wasm32::*;
        let chunks = a.len().min(b.len()) / 8;
        for i in 0..chunks {
            let offset = i * 8;
            let va = v128_load(a.as_ptr().add(offset).cast::<v128>());
            let vb = v128_load(b.as_ptr().add(offset).cast::<v128>());
            v128_store(a.as_mut_ptr().add(offset).cast::<v128>(), i16x8_sub(va, vb));
        }
        for (a, &b) in a[chunks * 8..].iter_mut().zip(&b[chunks * 8..]) {
            *a = a.wrapping_sub(b);
        }
        return;
    }
    #[allow(unreachable_code)]
    for (a, &b) in a.iter_mut().zip(b) {
        *a = a.wrapping_sub(b);
    }
}
fn add_i32(a: &mut [i32], b: &[i32]) {
    for (a, &b) in a.iter_mut().zip(b) {
        *a = a.wrapping_add(b);
    }
}
fn sub_i32(a: &mut [i32], b: &[i32]) {
    for (a, &b) in a.iter_mut().zip(b) {
        *a = a.wrapping_sub(b);
    }
}

fn sqr_transform_with_threat(
    us: &[i16],
    us_threat: &[i16],
    them: &[i16],
    them_threat: &[i16],
    output: &mut [u8],
) {
    let half = us.len() / 2;
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    let mut processed = 0;
    #[cfg(not(all(target_arch = "wasm32", target_feature = "simd128")))]
    let processed = 0;

    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    unsafe {
        use std::arch::wasm32::*;
        let zero = i16x8_splat(0);
        let max127 = i16x8_splat(127);
        for (acc, threat, out_offset) in [(us, us_threat, 0usize), (them, them_threat, half)] {
            let mut offset = 0;
            while offset + 16 <= half {
                let a0 = i16x8_add(
                    v128_load(acc.as_ptr().add(offset).cast::<v128>()),
                    v128_load(threat.as_ptr().add(offset).cast::<v128>()),
                );
                let b0 = i16x8_add(
                    v128_load(acc.as_ptr().add(half + offset).cast::<v128>()),
                    v128_load(threat.as_ptr().add(half + offset).cast::<v128>()),
                );
                let a1 = i16x8_add(
                    v128_load(acc.as_ptr().add(offset + 8).cast::<v128>()),
                    v128_load(threat.as_ptr().add(offset + 8).cast::<v128>()),
                );
                let b1 = i16x8_add(
                    v128_load(acc.as_ptr().add(half + offset + 8).cast::<v128>()),
                    v128_load(threat.as_ptr().add(half + offset + 8).cast::<v128>()),
                );
                let product0 = u16x8_shr(
                    i16x8_mul(
                        i16x8_min(i16x8_max(a0, zero), max127),
                        i16x8_min(i16x8_max(b0, zero), max127),
                    ),
                    7,
                );
                let product1 = u16x8_shr(
                    i16x8_mul(
                        i16x8_min(i16x8_max(a1, zero), max127),
                        i16x8_min(i16x8_max(b1, zero), max127),
                    ),
                    7,
                );
                v128_store(
                    output.as_mut_ptr().add(out_offset + offset).cast::<v128>(),
                    u8x16_narrow_i16x8(product0, product1),
                );
                offset += 16;
            }
            processed = offset;
        }
    }

    for i in processed..half {
        let us0 = us[i].wrapping_add(us_threat[i]);
        let us1 = us[half + i].wrapping_add(us_threat[half + i]);
        let them0 = them[i].wrapping_add(them_threat[i]);
        let them1 = them[half + i].wrapping_add(them_threat[half + i]);
        output[i] = ((u32::from(us0.clamp(0, 127) as u16) * u32::from(us1.clamp(0, 127) as u16))
            >> 7) as u8;
        output[half + i] = ((u32::from(them0.clamp(0, 127) as u16)
            * u32::from(them1.clamp(0, 127) as u16))
            >> 7) as u8;
    }
}
fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

#[cfg(all(test, feature = "layerstack-arch"))]
mod tests {
    use std::fs::File;
    use std::io::BufReader;

    use super::*;
    use crate::nnue::network::set_layer_stack_bucket_mode;
    use crate::nnue::network_layer_stacks::LayerStacksNetwork;
    use crate::position::SFEN_HIRATE;
    use crate::types::Move;

    #[test]
    #[ignore]
    fn real_model_matches_const_generic() {
        let path = std::env::var("NNUE_DYNAMIC_LS_COMPARE_FILE")
            .expect("set NNUE_DYNAMIC_LS_COMPARE_FILE to a LayerStacks NNUE file");
        let mut dynamic_reader = BufReader::new(File::open(&path).unwrap());
        let dynamic = DynamicLayerStacksNetwork::read(&mut dynamic_reader, None).unwrap();

        let mut static_reader = BufReader::new(File::open(&path).unwrap());
        let static_net = LayerStacksNetwork::read_with_options(
            &mut static_reader,
            dynamic.spec.l1,
            dynamic.spec.l2,
            dynamic.spec.l3,
            None,
        )
        .unwrap();

        set_layer_stack_bucket_mode(LayerStackBucketMode::KingRank9);
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        let mut dynamic_stack = dynamic.new_stack();
        dynamic.refresh(&pos, &mut dynamic_stack);
        let dynamic_value = dynamic.evaluate(&pos, &mut dynamic_stack);

        let mut static_stack = static_net.new_acc_stack();
        static_net.update_accumulator(&pos, &mut static_stack, &mut None);
        let static_value = static_net.evaluate(&pos, &static_stack);
        assert_eq!(dynamic_value, static_value);

        for move_text in ["7g7f", "3c3d", "2g2f", "8c8d", "6i7h", "4a3b"] {
            let mv = Move::from_usi(move_text).unwrap();
            let gives_check = pos.gives_check(mv);
            let dirty = pos.do_move(mv, gives_check);
            dynamic_stack.push(dirty);
            dynamic.ensure(&pos, &mut dynamic_stack);
            let incremental = dynamic.evaluate(&pos, &mut dynamic_stack);

            let mut refreshed_stack = dynamic.new_stack();
            dynamic.refresh(&pos, &mut refreshed_stack);
            let refreshed = dynamic.evaluate(&pos, &mut refreshed_stack);
            assert_eq!(incremental, refreshed, "dynamic update mismatch after {move_text}");
        }
    }
}
