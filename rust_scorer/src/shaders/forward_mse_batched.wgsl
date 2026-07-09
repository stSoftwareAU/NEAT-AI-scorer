// Forward-only MLP activation + per-record MSE for many creatures per dispatch.
//
// Issue #82 — multi-creature batched dispatch.
//
// Each thread handles a (creature, record) pair. The 2D dispatch grid is
// (records, creatures, 1); workgroup_size is (64, 1, 1). Per-creature partial
// sums are reduced across records via a workgroup-shared reduction in
// `partials[creature * num_workgroups_x + workgroup_x]`; the host sums the
// partials into per-creature totals after readback.
//
// Memory layout (all little-endian f32 except `Header`):
//
//   records    : array<f32>  — flat records [in0..inN, out0..outM, in0..inN, ...]
//   neurons    : array<NeuronGpu>  — one entry per non-input neuron, all creatures
//                                    concatenated in topological order
//   synapses   : array<SynapseGpu>  — one entry per synapse, all creatures concatenated
//   creatures  : array<CreatureMeta> — per-creature offsets into neurons/synapses
//   header     : Header — global record/creature counts
//   partials   : array<f32>  — output, length = num_creatures * num_workgroups_x
//
// Squash type encoding matches `neat_core::squash::SquashType`. Point-wise
// activations 0..=31 are inlined (Issue #305), matching `apply_squash` (the
// f32 CPU path) followed by `apply_limit_range`. The aggregate squashes
// MINIMUM (32) / MAXIMUM (33) / IF (34) are now hosted too (Issue #312): they
// reduce the individual weighted inputs (min / max / synapse-type branch)
// instead of summing them, matching
// `neat_core::batch_scoring::neuron_activation_scalar`. The remaining
// aggregates HYPOT (35) / HYPOTv2 (36) / MEAN (37) are still unsupported and
// force a CPU fallback via the host `squash_supported`. Constant neurons —
// which return a clamped bias and ignore their synapses — are hosted as well
// (Issue #312), flagged per-neuron by `NeuronGpu.is_constant`.

const WG_SIZE: u32 = 64u;

struct Header {
    num_records: u32,
    num_creatures: u32,
    num_inputs: u32,
    num_outputs: u32,
    values_per_record: u32,
    num_workgroups_x: u32,
    _pad0: u32,
    _pad1: u32,
}

struct NeuronGpu {
    bias: f32,
    squash_type: u32,
    start_synapse: u32,
    num_synapses: u32,
    // Non-zero for a constant neuron: returns a clamped bias and ignores its
    // synapses, matching the CPU `is_constant` branch (Issue #312).
    is_constant: u32,
}

struct SynapseGpu {
    weight: f32,
    from_index: u32,
    // `neat_core::synapse_type::SynapseType` discriminant, needed by the IF
    // aggregate to bucket inputs into condition/positive/negative (Issue #312).
    synapse_type: u32,
}

struct CreatureMeta {
    // Offset into `neurons` for the first non-input neuron of this creature.
    neuron_offset: u32,
    num_non_inputs: u32,
    // Offset into `synapses` for this creature's first synapse.
    synapse_offset: u32,
    // Per-creature activation scratch slice in `activations` (host pre-allocates
    // one slice per (workgroup_x, creature_idx) — but to keep GPU simple we
    // recompute activations entirely in private memory).
    num_neurons: u32,
}

@group(0) @binding(0) var<uniform> header: Header;
@group(0) @binding(1) var<storage, read> records: array<f32>;
@group(0) @binding(2) var<storage, read> neurons: array<NeuronGpu>;
@group(0) @binding(3) var<storage, read> synapses: array<SynapseGpu>;
@group(0) @binding(4) var<storage, read> creatures_buf: array<CreatureMeta>;
@group(0) @binding(5) var<storage, read_write> partials: array<f32>;

// Maximum supported neuron count per creature held in private activation
// scratch. The synthetic bench fixture has 18 neurons; production creatures
// up to ~256 neurons should fit. WGSL does not allow runtime-sized arrays in
// the `private` address space, so this is a fixed cap.
const MAX_NEURONS_PER_CREATURE: u32 = 256u;

// Constants mirror `neat_core::squash` / `neat_core::range` (Issue #305).
const SELU_ALPHA: f32 = 1.6732632;
const SELU_LAMBDA: f32 = 1.050701;
const GELU_COEFF: f32 = 0.044715;
const SQRT_2_OVER_PI: f32 = 0.7978846;
const LEAKY_RELU_ALPHA: f32 = 0.01;
const JS_MAX_SAFE_INTEGER: f32 = 9007199254740992.0;
// 0.99 minus 1 ulp — matches the CPU Softsign clamp (`f32::from_bits(..)-1`).
const SOFTSIGN_SQUASH_CLAMP: f32 = 0.98999995;
const SOFTSIGN_LIMIT: f32 = 0.99;
const FRAC_PI_2: f32 = 1.5707964;
const F32_LARGE: f32 = 3.4028235e38;
const GELU_MIN: f32 = -0.17;
const SWISH_MIN: f32 = -0.278;
const MISH_MIN: f32 = -0.309;
const SOFTPLUS_MIN: f32 = 1e-15;
const SOFTPLUS_MAX: f32 = 100.0;
const SELU_MIN: f32 = -(SELU_ALPHA * SELU_LAMBDA);

// Aggregate squash discriminants hosted since Issue #312 (mirror
// `neat_core::squash::SquashType`).
const SQUASH_MINIMUM: u32 = 32u;
const SQUASH_MAXIMUM: u32 = 33u;
const SQUASH_IF: u32 = 34u;
// `neat_core::synapse_type::SynapseType` discriminants used by the IF aggregate.
const SYN_CONDITION: u32 = 1u;
const SYN_NEGATIVE: u32 = 2u;

// Numerically stable softplus `ln(1 + e^x)` — never overflows for large x, so
// it matches the CPU f64 result where a naive f32 `exp(x)` would saturate.
fn softplus_stable(x: f32) -> f32 {
    return max(x, 0.0) + log(1.0 + exp(-abs(x)));
}

// Raw point-wise activation for discriminants 0..=31, mirroring
// `neat_core::squash::apply_squash` (the f32 CPU path). Transcendental inputs
// are clamped so Metal's `exp`/`tanh` cannot overflow to inf → NaN. Unknown /
// aggregate discriminants (filtered out on the host) fall through to IDENTITY.
fn raw_squash(t: u32, x: f32) -> f32 {
    switch t {
        case 0u: { return x; }                               // IDENTITY
        case 1u: { return max(x, 0.0); }                     // RELU
        case 2u: { return clamp(x, 0.0, 6.0); }              // RELU6
        case 3u: { return select(LEAKY_RELU_ALPHA * x, x, x >= 0.0); } // LEAKY_RELU
        case 4u: {                                           // SELU
            let sx = min(x, 709.0);
            let fx = select(SELU_ALPHA * exp(sx) - SELU_ALPHA, sx, sx > 0.0);
            return SELU_LAMBDA * fx;
        }
        case 5u: { return select(exp(x) - 1.0, x, x > 0.0); } // ELU
        case 6u: { return 1.0 / (1.0 + exp(-clamp(x, -30.0, 30.0))); } // LOGISTIC
        case 7u: { return tanh(clamp(x, -30.0, 30.0)); }     // TANH
        case 8u: { return clamp(x, -1.0, 1.0); }             // HARD_TANH
        case 9u: {                                           // SOFTSIGN
            let y = x / (1.0 + abs(x));
            return clamp(y, -SOFTSIGN_SQUASH_CLAMP, SOFTSIGN_SQUASH_CLAMP);
        }
        case 10u: {                                          // SOFTPLUS
            if x >= 709.0 { return SOFTPLUS_MAX; }
            return softplus_stable(x);
        }
        case 11u: { return x / (1.0 + exp(-clamp(x, -30.0, 30.0))); } // SWISH
        case 12u: { return x * tanh(softplus_stable(x)); }   // MISH
        case 13u: {                                          // GELU (tanh approx)
            let inner = clamp(SQRT_2_OVER_PI * (x + GELU_COEFF * x * x * x), -30.0, 30.0);
            return 0.5 * x * (1.0 + tanh(inner));
        }
        case 14u: { return sin(x); }                         // SINE
        case 15u: { return cos(x); }                         // COSINE
        case 16u: {                                          // TAN (clamped)
            let tv = tan(x);
            if tv != tv { return 0.0; }
            return clamp(tv, -1000.0, 1000.0);
        }
        case 17u: { return atan(x); }                        // ARCTAN
        case 18u: { return exp(-x * x); }                    // GAUSSIAN
        case 19u: { return (sqrt(x * x + 1.0) - 1.0) * 0.5 + x; } // BENT_IDENTITY
        case 20u: { return 2.0 / (1.0 + exp(-clamp(x, -30.0, 30.0))) - 1.0; } // BIPOLAR_SIGMOID
        case 21u: { return select(-1.0, 1.0, x > 0.0); }     // BIPOLAR
        case 22u: { return select(0.0, 1.0, x > 0.0); }      // STEP
        case 23u: { return 1.0 - x; }                        // COMPLEMENT
        case 24u: { return abs(x); }                         // ABSOLUTE
        case 25u: { return min(x * x, 1.0e6); }              // SQUARE (clamped)
        case 26u: { return clamp(x * x * x, -1.0e6, 1.0e6); } // CUBE (clamped)
        case 27u: { return select(0.0, sqrt(x), x >= 0.0); } // SQRT
        case 28u: {                                          // STD_INVERSE
            if abs(x) < 1.0e-10 { return select(-1.0e10, 1.0e10, x >= 0.0); }
            return 1.0 / x;
        }
        case 29u: {                                          // EXPONENTIAL (clamped)
            if x >= 36.0 { return JS_MAX_SAFE_INTEGER; }
            return exp(x);
        }
        case 30u: {                                          // LOG_SIGMOID
            if x <= -709.0 { return -JS_MAX_SAFE_INTEGER; }
            return -softplus_stable(-x);
        }
        case 31u: { return x / sqrt(1.0 + x * x); }          // ISRU
        default: { return x; }
    }
}

// Output range (low, high) per squash, mirroring
// `neat_core::range::apply_get_range`.
fn range_of(t: u32) -> vec2<f32> {
    switch t {
        case 1u, 24u, 25u, 27u, 29u: { return vec2<f32>(0.0, F32_LARGE); }
        case 6u, 18u, 22u: { return vec2<f32>(0.0, 1.0); }
        case 7u, 8u, 14u, 15u, 20u, 21u, 31u: { return vec2<f32>(-1.0, 1.0); }
        case 2u: { return vec2<f32>(0.0, 6.0); }
        case 9u: { return vec2<f32>(-SOFTSIGN_LIMIT, SOFTSIGN_LIMIT); }
        case 10u: { return vec2<f32>(SOFTPLUS_MIN, SOFTPLUS_MAX); }
        case 17u: { return vec2<f32>(-FRAC_PI_2, FRAC_PI_2); }
        case 5u: { return vec2<f32>(-1.0, F32_LARGE); }
        case 4u: { return vec2<f32>(SELU_MIN, F32_LARGE); }
        case 30u: { return vec2<f32>(-F32_LARGE, 0.0); }
        case 11u: { return vec2<f32>(SWISH_MIN, F32_LARGE); }
        case 12u: { return vec2<f32>(MISH_MIN, F32_LARGE); }
        case 13u: { return vec2<f32>(GELU_MIN, F32_LARGE); }
        default: { return vec2<f32>(-F32_LARGE, F32_LARGE); }
    }
}

// Range-limit a raw activation (NaN -> 0, infinities clamped to the finite
// bounds), mirroring `neat_core::range::apply_limit_range`. Shared by the
// point-wise `activate` and the aggregate reductions (Issue #312).
fn limit_range(t: u32, raw: f32) -> f32 {
    if raw != raw { return 0.0; }
    let r = range_of(t);
    return clamp(raw, r.x, r.y);
}

// Full point-wise activation: raw squash then range-limit, matching the CPU
// `apply_squash` + `apply_limit_range` pipeline (Issue #305).
fn activate(t: u32, x: f32) -> f32 {
    return limit_range(t, raw_squash(t, x));
}

var<workgroup> wg_partial: array<f32, WG_SIZE>;

@compute @workgroup_size(WG_SIZE, 1, 1)
fn forward_mse_batched(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wgid: vec3<u32>,
) {
    let record_idx = gid.x;
    let creature_idx = gid.y;
    let local_idx = lid.x;

    // Activation scratch in private memory. Filled with zeros once per thread.
    var act: array<f32, MAX_NEURONS_PER_CREATURE>;

    var sample_err: f32 = 0.0;
    var thread_active = false;

    if (record_idx < header.num_records && creature_idx < header.num_creatures) {
        thread_active = true;
        let cr = creatures_buf[creature_idx];
        let rec_base = record_idx * header.values_per_record;

        // Initialise input activations from the record's input slice.
        for (var i: u32 = 0u; i < header.num_inputs; i = i + 1u) {
            act[i] = records[rec_base + i];
        }
        // Zero the rest of the activation buffer up to num_neurons.
        for (var i: u32 = header.num_inputs; i < cr.num_neurons; i = i + 1u) {
            act[i] = 0.0;
        }

        // Compute non-input neurons in topological order.
        for (var n: u32 = 0u; n < cr.num_non_inputs; n = n + 1u) {
            let neuron = neurons[cr.neuron_offset + n];
            let syn_base = cr.synapse_offset + neuron.start_synapse;
            var a: f32;
            if (neuron.is_constant != 0u) {
                // Constant neuron: clamped bias, synapses ignored (Issue #312).
                a = limit_range(0u, neuron.bias); // IDENTITY range
            } else if (neuron.squash_type == SQUASH_MINIMUM) {
                let inf = bitcast<f32>(0x7f800000u);
                var m: f32 = inf;
                for (var s: u32 = 0u; s < neuron.num_synapses; s = s + 1u) {
                    let syn = synapses[syn_base + s];
                    let val = act[syn.from_index] * syn.weight;
                    if (val < m) { m = val; }
                }
                let raw = select(m + neuron.bias, neuron.bias, m == inf);
                a = limit_range(neuron.squash_type, raw);
            } else if (neuron.squash_type == SQUASH_MAXIMUM) {
                let ninf = bitcast<f32>(0xff800000u);
                var m: f32 = ninf;
                for (var s: u32 = 0u; s < neuron.num_synapses; s = s + 1u) {
                    let syn = synapses[syn_base + s];
                    let val = act[syn.from_index] * syn.weight;
                    if (val > m) { m = val; }
                }
                let raw = select(m + neuron.bias, neuron.bias, m == ninf);
                a = limit_range(neuron.squash_type, raw);
            } else if (neuron.squash_type == SQUASH_IF) {
                var cond: f32 = 0.0;
                var pos: f32 = 0.0;
                var neg: f32 = 0.0;
                for (var s: u32 = 0u; s < neuron.num_synapses; s = s + 1u) {
                    let syn = synapses[syn_base + s];
                    let val = act[syn.from_index] * syn.weight;
                    if (syn.synapse_type == SYN_CONDITION) {
                        cond = cond + val;
                    } else if (syn.synapse_type == SYN_NEGATIVE) {
                        neg = neg + val;
                    } else {
                        pos = pos + val;
                    }
                }
                let raw = select(neg + neuron.bias, pos + neuron.bias, cond > 0.0);
                a = limit_range(neuron.squash_type, raw);
            } else {
                var z: f32 = neuron.bias;
                for (var s: u32 = 0u; s < neuron.num_synapses; s = s + 1u) {
                    let syn = synapses[syn_base + s];
                    z = z + syn.weight * act[syn.from_index];
                }
                a = activate(neuron.squash_type, z);
            }
            act[header.num_inputs + n] = a;
        }

        // Per-record squared error = mean over outputs of (target - predicted)^2.
        let output_start = cr.num_neurons - header.num_outputs;
        let target_start = rec_base + header.num_inputs;
        var sq_sum: f32 = 0.0;
        for (var o: u32 = 0u; o < header.num_outputs; o = o + 1u) {
            let target_val = records[target_start + o];
            let predicted = act[output_start + o];
            let d = target_val - predicted;
            sq_sum = sq_sum + d * d;
        }
        if (header.num_outputs > 0u) {
            sample_err = sq_sum / f32(header.num_outputs);
        }
    }

    // Workgroup reduction across records (lid.x in [0, WG_SIZE)).
    if (thread_active) {
        wg_partial[local_idx] = sample_err;
    } else {
        wg_partial[local_idx] = 0.0;
    }
    workgroupBarrier();

    var stride: u32 = WG_SIZE / 2u;
    loop {
        if (stride == 0u) { break; }
        if (local_idx < stride) {
            wg_partial[local_idx] = wg_partial[local_idx] + wg_partial[local_idx + stride];
        }
        workgroupBarrier();
        stride = stride / 2u;
    }

    if (local_idx == 0u) {
        let out_idx = creature_idx * header.num_workgroups_x + wgid.x;
        partials[out_idx] = wg_partial[0];
    }
}
