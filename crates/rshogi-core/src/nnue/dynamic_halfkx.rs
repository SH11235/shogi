//! Runtime-dimension HalfKX inference used by `edition-universal`.
//!
//! Fixed editions deliberately keep using the const-generic implementations.

use std::io::{self, Read, Seek, SeekFrom};

use super::accumulator::{
    AlignedBox, DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES,
};
use super::activation::{CReLU, FtActivation, PairwiseCReLU, SCReLU, default_qa_for_arch};
use super::constants::{
    FV_SCALE, FV_SCALE_HALFKA, MAX_ARCH_LEN, NNUE_VERSION, NNUE_VERSION_HALFKA,
};
use super::features::{
    FeatureSet as FeatureSetTrait, HalfKPFeatureSet, HalfKaHmMergedFeatureSet,
    HalfKaHmSplitFeatureSet, HalfKaMergedFeatureSet, HalfKaSplitFeatureSet,
};
use super::layers::padded_input;
use super::network::{get_fv_scale_override, parse_fv_scale_from_arch};
use super::spec::{
    Activation, ArchitectureSpec, FeatureSet, parse_architecture, parse_feature_input_dimensions,
};
use crate::position::Position;
use crate::types::{Color, Value};

const STACK_CAPACITY: usize = 256;
const MAX_RUNTIME_DIMENSION: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeFeatureSet {
    HalfKP,
    HalfKaHmMerged,
    HalfKaSplit,
    HalfKaMerged,
    HalfKaHmSplit,
}

impl RuntimeFeatureSet {
    fn from_spec(feature_set: FeatureSet) -> io::Result<Self> {
        match feature_set {
            FeatureSet::HalfKP => Ok(Self::HalfKP),
            FeatureSet::HalfKaHmMerged => Ok(Self::HalfKaHmMerged),
            FeatureSet::HalfKaSplit => Ok(Self::HalfKaSplit),
            FeatureSet::HalfKaMerged => Ok(Self::HalfKaMerged),
            FeatureSet::HalfKaHmSplit => Ok(Self::HalfKaHmSplit),
            _ => Err(invalid_data(format!("{feature_set} is not a HalfKX feature set"))),
        }
    }

    fn dimensions(self) -> usize {
        match self {
            Self::HalfKP => HalfKPFeatureSet::DIMENSIONS,
            Self::HalfKaHmMerged => HalfKaHmMergedFeatureSet::DIMENSIONS,
            Self::HalfKaSplit => HalfKaSplitFeatureSet::DIMENSIONS,
            Self::HalfKaMerged => HalfKaMergedFeatureSet::DIMENSIONS,
            Self::HalfKaHmSplit => HalfKaHmSplitFeatureSet::DIMENSIONS,
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
        }
    }

    fn changed(
        self,
        dirty: &DirtyPiece,
        perspective: Color,
        pos: &Position,
    ) -> (IndexList<MAX_CHANGED_FEATURES>, IndexList<MAX_CHANGED_FEATURES>) {
        let king_sq = pos.king_square(perspective);
        match self {
            Self::HalfKP => HalfKPFeatureSet::collect_changed_indices(dirty, perspective, king_sq),
            Self::HalfKaHmMerged => {
                HalfKaHmMergedFeatureSet::collect_changed_indices(dirty, perspective, king_sq)
            }
            Self::HalfKaSplit => {
                HalfKaSplitFeatureSet::collect_changed_indices(dirty, perspective, king_sq)
            }
            Self::HalfKaMerged => {
                HalfKaMergedFeatureSet::collect_changed_indices(dirty, perspective, king_sq)
            }
            Self::HalfKaHmSplit => {
                HalfKaHmSplitFeatureSet::collect_changed_indices(dirty, perspective, king_sq)
            }
        }
    }

    fn needs_refresh(self, dirty: &DirtyPiece, perspective: Color) -> bool {
        match self {
            Self::HalfKP => HalfKPFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaHmMerged => HalfKaHmMergedFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaSplit => HalfKaSplitFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaMerged => HalfKaMergedFeatureSet::needs_refresh(dirty, perspective),
            Self::HalfKaHmSplit => HalfKaHmSplitFeatureSet::needs_refresh(dirty, perspective),
        }
    }
}

struct DynamicAffine {
    input_dim: usize,
    padded_input: usize,
    output_dim: usize,
    biases: AlignedBox<i32>,
    weights: AlignedBox<i8>,
}

impl DynamicAffine {
    fn read<R: Read>(reader: &mut R, input_dim: usize, output_dim: usize) -> io::Result<Self> {
        let padded_input = padded_input(input_dim);
        let mut biases = AlignedBox::new_zeroed(output_dim);
        let mut buf4 = [0; 4];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf4)?;
            *bias = i32::from_le_bytes(buf4);
        }
        let weight_len = output_dim
            .checked_mul(padded_input)
            .ok_or_else(|| invalid_data("affine weight dimensions overflow"))?;
        let mut weights = AlignedBox::new_zeroed(weight_len);
        let mut byte = [0];
        for weight in weights.iter_mut() {
            reader.read_exact(&mut byte)?;
            *weight = byte[0] as i8;
        }
        Ok(Self {
            input_dim,
            padded_input,
            output_dim,
            biases,
            weights,
        })
    }

    #[inline]
    fn propagate(&self, input: &[u8], output: &mut [i32]) {
        debug_assert!(input.len() >= self.padded_input);
        debug_assert!(output.len() >= self.output_dim);
        for (o, out) in output[..self.output_dim].iter_mut().enumerate() {
            let row = &self.weights[o * self.padded_input..(o + 1) * self.padded_input];
            let mut sum = self.biases[o];
            for (&x, &w) in input[..self.input_dim].iter().zip(row) {
                sum = sum.wrapping_add(i32::from(x) * i32::from(w));
            }
            *out = sum;
        }
    }
}

/// Runtime-dimension HalfKX network.
pub struct DynamicHalfKxNetwork {
    spec: ArchitectureSpec,
    feature_set: RuntimeFeatureSet,
    input_dimensions: usize,
    activation: Activation,
    ft_biases: AlignedBox<i16>,
    ft_weights: AlignedBox<i16>,
    l1: DynamicAffine,
    l2: DynamicAffine,
    output: DynamicAffine,
    fv_scale: i32,
    qa: i16,
}

impl DynamicHalfKxNetwork {
    pub(crate) fn read<R: Read + Seek>(reader: &mut R) -> io::Result<Self> {
        let file_size = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;

        let mut buf4 = [0; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);
        if version != NNUE_VERSION && version != NNUE_VERSION_HALFKA {
            return Err(invalid_data(format!("unsupported HalfKX NNUE version: {version:#x}")));
        }
        reader.read_exact(&mut buf4)?; // structure hash
        reader.read_exact(&mut buf4)?;
        let arch_len = u32::from_le_bytes(buf4) as usize;
        if arch_len == 0 || arch_len > MAX_ARCH_LEN {
            return Err(invalid_data(format!("invalid architecture string length: {arch_len}")));
        }
        let mut arch = vec![0; arch_len];
        reader.read_exact(&mut arch)?;
        let arch = std::str::from_utf8(&arch)
            .map_err(|_| invalid_data("architecture string is not valid UTF-8"))?;
        if arch.contains("Factorizer") {
            return Err(invalid_data(
                "factorized training models must be coalesced before inference",
            ));
        }

        let parsed = parse_architecture(arch).map_err(invalid_data)?;
        let feature_set = RuntimeFeatureSet::from_spec(parsed.feature_set)?;
        validate_dimension("l1", parsed.l1)?;
        validate_dimension("l2", parsed.l2)?;
        validate_dimension("l3", parsed.l3)?;
        let input_dimensions = parse_feature_input_dimensions(arch)
            .ok_or_else(|| invalid_data("HalfKX architecture is missing FT input dimensions"))?;
        if input_dimensions != feature_set.dimensions() {
            return Err(invalid_data(format!(
                "FT input dimension mismatch: header={input_dimensions}, feature_set={} expects {}",
                parsed.feature_set,
                feature_set.dimensions()
            )));
        }
        let activation = if arch.contains("PairwiseCReLU") || arch.contains("-Pairwise") {
            Activation::PairwiseCReLU
        } else if arch.contains("SCReLU") {
            Activation::SCReLU
        } else {
            Activation::CReLU
        };
        let dense_input = parsed
            .l1
            .checked_mul(2)
            .and_then(|v| v.checked_div(activation.output_dim_divisor()))
            .ok_or_else(|| invalid_data("invalid FT output dimensions"))?;

        reader.read_exact(&mut buf4)?; // feature transformer hash
        let mut ft_biases = AlignedBox::new_zeroed(parsed.l1);
        let mut buf2 = [0; 2];
        for bias in ft_biases.iter_mut() {
            reader.read_exact(&mut buf2)?;
            *bias = i16::from_le_bytes(buf2);
        }
        let ft_weight_len = input_dimensions
            .checked_mul(parsed.l1)
            .ok_or_else(|| invalid_data("FT weight dimensions overflow"))?;
        let mut ft_weights = AlignedBox::new_zeroed(ft_weight_len);
        for weight in ft_weights.iter_mut() {
            reader.read_exact(&mut buf2)?;
            *weight = i16::from_le_bytes(buf2);
        }

        reader.read_exact(&mut buf4)?; // dense network hash
        let l1 = DynamicAffine::read(reader, dense_input, parsed.l2)?;
        let l2 = DynamicAffine::read(reader, parsed.l2, parsed.l3)?;
        let output = DynamicAffine::read(reader, parsed.l3, 1)?;
        let consumed = reader.stream_position()?;
        if consumed != file_size {
            return Err(invalid_data(format!(
                "NNUE payload size mismatch: consumed={consumed}, file_size={file_size}"
            )));
        }

        let fv_scale =
            parse_fv_scale_from_arch(arch).unwrap_or(if parsed.feature_set == FeatureSet::HalfKP {
                FV_SCALE
            } else {
                FV_SCALE_HALFKA
            });
        let qa = parse_qa(arch).unwrap_or_else(|| default_qa_for_arch(arch));
        Ok(Self {
            spec: ArchitectureSpec::new(
                parsed.feature_set,
                parsed.l1,
                parsed.l2,
                parsed.l3,
                activation,
            ),
            feature_set,
            input_dimensions,
            activation,
            ft_biases,
            ft_weights,
            l1,
            l2,
            output,
            fv_scale,
            qa,
        })
    }

    pub(crate) fn spec(&self) -> ArchitectureSpec {
        self.spec
    }
    pub(crate) fn l1_size(&self) -> usize {
        self.spec.l1
    }
    pub(crate) fn is_halfkp(&self) -> bool {
        self.spec.feature_set == FeatureSet::HalfKP
    }

    pub(crate) fn refresh(&self, pos: &Position, stack: &mut DynamicHalfKxStack) {
        for perspective in [Color::Black, Color::White] {
            self.refresh_perspective(pos, perspective, stack.current_accumulation_mut(perspective));
        }
        stack.computed[stack.current] = true;
    }

    fn refresh_perspective(&self, pos: &Position, perspective: Color, accumulation: &mut [i16]) {
        accumulation.copy_from_slice(&self.ft_biases);
        for index in self.feature_set.active(pos, perspective).iter() {
            add_row(accumulation, self.ft_row(index));
        }
    }

    pub(crate) fn ensure(&self, pos: &Position, stack: &mut DynamicHalfKxStack) {
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
            let p = perspective.index();
            if self.feature_set.needs_refresh(&dirty, perspective) {
                self.refresh_perspective(
                    pos,
                    perspective,
                    stack.current_accumulation_mut(perspective),
                );
                continue;
            }
            let (removed, added) = self.feature_set.changed(&dirty, perspective, pos);
            let l1 = self.spec.l1;
            let prev_start = ((current - 1) * 2 + p) * l1;
            let curr_start = (current * 2 + p) * l1;
            let (before, after) = stack.accumulations.split_at_mut(curr_start);
            let prev = &before[prev_start..prev_start + l1];
            let curr = &mut after[..l1];
            curr.copy_from_slice(prev);
            for index in removed.iter() {
                sub_row(curr, self.ft_row(index));
            }
            for index in added.iter() {
                add_row(curr, self.ft_row(index));
            }
        }
        stack.computed[current] = true;
    }

    pub(crate) fn evaluate(&self, pos: &Position, stack: &mut DynamicHalfKxStack) -> Value {
        let l1 = self.spec.l1;
        let stm = pos.side_to_move().index();
        let opp = (!pos.side_to_move()).index();
        let base = stack.current * 2 * l1;
        stack.ft_raw[..l1]
            .copy_from_slice(&stack.accumulations[base + stm * l1..base + (stm + 1) * l1]);
        stack.ft_raw[l1..2 * l1]
            .copy_from_slice(&stack.accumulations[base + opp * l1..base + (opp + 1) * l1]);
        stack.ft_activated.fill(0);
        activate_i16(
            self.activation,
            &stack.ft_raw[..2 * l1],
            &mut stack.ft_activated[..self.l1.input_dim],
            self.qa,
        );
        self.l1.propagate(&stack.ft_activated, &mut stack.layer_i32);
        stack.layer_u8.fill(0);
        activate_i32(
            self.activation,
            &stack.layer_i32[..self.spec.l2],
            &mut stack.layer_u8[..self.spec.l2],
        );
        self.l2.propagate(&stack.layer_u8, &mut stack.layer2_i32);
        stack.layer2_u8.fill(0);
        activate_i32(
            self.activation,
            &stack.layer2_i32[..self.spec.l3],
            &mut stack.layer2_u8[..self.spec.l3],
        );
        let mut output = [0];
        self.output.propagate(&stack.layer2_u8, &mut output);
        Value::new(output[0] / get_fv_scale_override().unwrap_or(self.fv_scale))
    }

    #[inline]
    fn ft_row(&self, index: usize) -> &[i16] {
        debug_assert!(index < self.input_dimensions);
        let start = index * self.spec.l1;
        &self.ft_weights[start..start + self.spec.l1]
    }
}

/// Per-search runtime accumulator and preallocated inference scratch.
pub struct DynamicHalfKxStack {
    l1: usize,
    current: usize,
    accumulations: AlignedBox<i16>,
    computed: Vec<bool>,
    dirty: Vec<DirtyPiece>,
    ft_raw: AlignedBox<i16>,
    ft_activated: AlignedBox<u8>,
    layer_i32: AlignedBox<i32>,
    layer_u8: AlignedBox<u8>,
    layer2_i32: AlignedBox<i32>,
    layer2_u8: AlignedBox<u8>,
}

impl DynamicHalfKxStack {
    pub(crate) fn new(net: &DynamicHalfKxNetwork) -> Self {
        Self {
            l1: net.spec.l1,
            current: 0,
            accumulations: AlignedBox::new_zeroed(STACK_CAPACITY * 2 * net.spec.l1),
            computed: vec![false; STACK_CAPACITY],
            dirty: (0..STACK_CAPACITY).map(|_| DirtyPiece::default()).collect(),
            ft_raw: AlignedBox::new_zeroed(2 * net.spec.l1),
            ft_activated: AlignedBox::new_zeroed(net.l1.padded_input),
            layer_i32: AlignedBox::new_zeroed(net.spec.l2),
            layer_u8: AlignedBox::new_zeroed(padded_input(net.spec.l2)),
            layer2_i32: AlignedBox::new_zeroed(net.spec.l3),
            layer2_u8: AlignedBox::new_zeroed(padded_input(net.spec.l3)),
        }
    }

    pub(crate) fn l1_size(&self) -> usize {
        self.l1
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
    fn current_accumulation_mut(&mut self, perspective: Color) -> &mut [i16] {
        let start = (self.current * 2 + perspective.index()) * self.l1;
        &mut self.accumulations[start..start + self.l1]
    }
}

fn activate_i16(kind: Activation, input: &[i16], output: &mut [u8], qa: i16) {
    match kind {
        Activation::CReLU => CReLU::activate_i16_to_u8(input, output, qa),
        Activation::SCReLU => SCReLU::activate_i16_to_u8(input, output, qa),
        Activation::PairwiseCReLU => PairwiseCReLU::activate_i16_to_u8(input, output, qa),
    }
}

fn activate_i32(kind: Activation, input: &[i32], output: &mut [u8]) {
    match kind {
        Activation::CReLU => CReLU::activate_i32_to_u8(input, output),
        Activation::SCReLU => SCReLU::activate_i32_to_u8(input, output),
        Activation::PairwiseCReLU => PairwiseCReLU::activate_i32_to_u8(input, output),
    }
}

#[inline]
fn add_row(acc: &mut [i16], row: &[i16]) {
    for (a, &w) in acc.iter_mut().zip(row) {
        *a = a.wrapping_add(w);
    }
}

#[inline]
fn sub_row(acc: &mut [i16], row: &[i16]) {
    for (a, &w) in acc.iter_mut().zip(row) {
        *a = a.wrapping_sub(w);
    }
}

fn parse_qa(arch: &str) -> Option<i16> {
    arch.split(',').find_map(|part| part.strip_prefix("qa=")?.parse().ok())
}

fn validate_dimension(name: &str, value: usize) -> io::Result<()> {
    if value == 0 || value > MAX_RUNTIME_DIMENSION {
        Err(invalid_data(format!("invalid runtime {name} dimension: {value}")))
    } else {
        Ok(())
    }
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::BufReader;

    use super::*;
    use crate::nnue::halfkp::{HalfKPNetwork, HalfKPStack};
    use crate::position::SFEN_HIRATE;

    /// 実ファイルを dynamic / const-generic の両方で読み、評価値を比較する。
    ///
    /// `NNUE_DYNAMIC_COMPARE_FILE` に既知形状の HalfKP net を指定して実行する。
    #[test]
    #[ignore]
    fn real_halfkp_matches_const_generic() {
        let path = std::env::var("NNUE_DYNAMIC_COMPARE_FILE")
            .expect("set NNUE_DYNAMIC_COMPARE_FILE to a HalfKP NNUE file");

        let mut dynamic_reader = BufReader::new(File::open(&path).unwrap());
        let dynamic = DynamicHalfKxNetwork::read(&mut dynamic_reader).unwrap();
        assert_eq!(dynamic.spec.feature_set, FeatureSet::HalfKP);

        let mut static_reader = BufReader::new(File::open(&path).unwrap());
        let static_net = HalfKPNetwork::read(
            &mut static_reader,
            dynamic.spec.l1,
            dynamic.spec.l2,
            dynamic.spec.l3,
            dynamic.spec.activation,
        )
        .unwrap();

        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        let mut dynamic_stack = DynamicHalfKxStack::new(&dynamic);
        dynamic.refresh(&pos, &mut dynamic_stack);
        let dynamic_value = dynamic.evaluate(&pos, &mut dynamic_stack);

        let mut static_stack = HalfKPStack::from_network(&static_net);
        static_net.refresh_accumulator(&pos, &mut static_stack);
        let static_value = static_net.evaluate(&pos, &static_stack);
        assert_eq!(dynamic_value, static_value);
    }
}
