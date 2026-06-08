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
// Squash type encoding matches `neat_core::squash::SquashType`:
//   0 = IDENTITY, 1 = RELU, 6 = LOGISTIC, 7 = TANH
// Other squash types are not supported by this kernel — the host falls back
// to CPU when any creature uses them.

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
}

struct SynapseGpu {
    weight: f32,
    from_index: u32,
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

fn squash(t: u32, x: f32) -> f32 {
    // 0 = IDENTITY, 1 = RELU, 6 = LOGISTIC, 7 = TANH (other types are filtered
    // out by the host before dispatch; treat unknown values as IDENTITY).
    // Clamp before the transcendental so large pre-activations cannot overflow
    // Metal's `tanh`/`exp` to inf → NaN (Issue #182). Both functions are fully
    // saturated by |x| = 30 in f32, so this matches the CPU libm result.
    let c = clamp(x, -30.0, 30.0);
    if (t == 7u) {
        return tanh(c);
    } else if (t == 6u) {
        return 1.0 / (1.0 + exp(-c));
    } else if (t == 1u) {
        if (x > 0.0) { return x; } else { return 0.0; }
    }
    // IDENTITY (0) or unsupported.
    return x;
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
            var z: f32 = neuron.bias;
            for (var s: u32 = 0u; s < neuron.num_synapses; s = s + 1u) {
                let syn = synapses[cr.synapse_offset + neuron.start_synapse + s];
                z = z + syn.weight * act[syn.from_index];
            }
            let a = squash(neuron.squash_type, z);
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
