// Forward-only MLP activation + per-record MSE for *large* creatures.
//
// Issue #182 — lifts the 256-neuron cap of `forward_mse_batched.wgsl`.
//
// The original batched kernel holds each invocation's activations in a
// fixed-size `private` array, which WGSL requires to be a compile-time
// constant — hence the 256-neuron cap. Production creatures routinely exceed
// that (observed 4139 neurons), so this kernel moves the activation scratch
// into a runtime-sized `storage` buffer instead. Storage arrays *may* be
// runtime-sized, so the per-creature neuron count is no longer bounded by a
// compile-time constant.
//
// ## Bounded concurrency via a grid-stride loop
//
// A storage scratch slice is needed for every concurrently-live thread, so the
// host bounds the thread count with a memory budget and the kernel walks the
// records with a grid-stride loop. The dispatch grid is `(G_x, num_creatures,
// 1)` with workgroup size `(WG_SIZE, 1, 1)`; `G_x` (`header.num_workgroups_x`)
// is chosen by the host so `num_creatures * G_x * WG_SIZE * max_neurons` floats
// fit the scratch budget. Each thread owns scratch slot
// `((creature_idx * G_x + wgid.x) * WG_SIZE + lid.x)` and reuses it for every
// record it visits, so there is never any aliasing between threads.
//
// Per-creature partial sums reduce across the `G_x` workgroups exactly as in
// `forward_mse_batched` (`partials[creature * G_x + wgid.x]`); the host sums
// the `G_x` partials per creature after readback. The result is bit-comparable
// (within the #81/#82 tolerance) to both the CPU path and the small-creature
// kernel.
//
// Squash encoding matches `forward_mse_batched.wgsl`:
//   0 = IDENTITY, 1 = RELU, 6 = LOGISTIC, 7 = TANH

const WG_SIZE: u32 = 64u;

struct Header {
    num_records: u32,
    num_creatures: u32,
    num_inputs: u32,
    num_outputs: u32,
    values_per_record: u32,
    num_workgroups_x: u32,
    // Activation scratch stride per thread (>= every creature's num_neurons).
    max_neurons: u32,
    _pad0: u32,
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
    neuron_offset: u32,
    num_non_inputs: u32,
    synapse_offset: u32,
    num_neurons: u32,
}

@group(0) @binding(0) var<uniform> header: Header;
@group(0) @binding(1) var<storage, read> records: array<f32>;
@group(0) @binding(2) var<storage, read> neurons: array<NeuronGpu>;
@group(0) @binding(3) var<storage, read> synapses: array<SynapseGpu>;
@group(0) @binding(4) var<storage, read> creatures_buf: array<CreatureMeta>;
@group(0) @binding(5) var<storage, read_write> partials: array<f32>;
// Per-thread activation scratch, length = num_creatures * G_x * WG_SIZE * max_neurons.
@group(0) @binding(6) var<storage, read_write> scratch: array<f32>;

fn squash(t: u32, x: f32) -> f32 {
    // Clamp before the transcendental so large pre-activations cannot overflow
    // Metal's `tanh`/`exp` to inf → NaN (Issue #182). tanh/logistic are fully
    // saturated by |x| = 30 in f32, so clamping matches the CPU libm result.
    let c = clamp(x, -30.0, 30.0);
    if (t == 7u) {
        return tanh(c);
    } else if (t == 6u) {
        return 1.0 / (1.0 + exp(-c));
    } else if (t == 1u) {
        if (x > 0.0) { return x; } else { return 0.0; }
    }
    return x;
}

var<workgroup> wg_partial: array<f32, WG_SIZE>;

@compute @workgroup_size(WG_SIZE, 1, 1)
fn forward_mse_scratch(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wgid: vec3<u32>,
) {
    let creature_idx = gid.y;
    let local_idx = lid.x;

    var thread_sum: f32 = 0.0;

    if (creature_idx < header.num_creatures) {
        let cr = creatures_buf[creature_idx];

        // This thread's dedicated activation scratch slice.
        let thread_lin = (creature_idx * header.num_workgroups_x + wgid.x) * WG_SIZE + local_idx;
        let base = thread_lin * header.max_neurons;

        // Grid-stride over records: stride is the total thread count in x for
        // this creature row (G_x * WG_SIZE).
        let stride = header.num_workgroups_x * WG_SIZE;
        var record_idx = wgid.x * WG_SIZE + local_idx;
        loop {
            if (record_idx >= header.num_records) { break; }

            let rec_base = record_idx * header.values_per_record;

            // Initialise input activations, zero the remainder.
            for (var i: u32 = 0u; i < header.num_inputs; i = i + 1u) {
                scratch[base + i] = records[rec_base + i];
            }
            for (var i: u32 = header.num_inputs; i < cr.num_neurons; i = i + 1u) {
                scratch[base + i] = 0.0;
            }

            // Forward pass over non-input neurons in topological order.
            for (var n: u32 = 0u; n < cr.num_non_inputs; n = n + 1u) {
                let neuron = neurons[cr.neuron_offset + n];
                var z: f32 = neuron.bias;
                for (var s: u32 = 0u; s < neuron.num_synapses; s = s + 1u) {
                    let syn = synapses[cr.synapse_offset + neuron.start_synapse + s];
                    z = z + syn.weight * scratch[base + syn.from_index];
                }
                scratch[base + header.num_inputs + n] = squash(neuron.squash_type, z);
            }

            // Per-record MSE = mean over outputs of (target - predicted)^2.
            let output_start = cr.num_neurons - header.num_outputs;
            let target_start = rec_base + header.num_inputs;
            var sq_sum: f32 = 0.0;
            for (var o: u32 = 0u; o < header.num_outputs; o = o + 1u) {
                let d = records[target_start + o] - scratch[base + output_start + o];
                sq_sum = sq_sum + d * d;
            }
            if (header.num_outputs > 0u) {
                thread_sum = thread_sum + sq_sum / f32(header.num_outputs);
            }

            record_idx = record_idx + stride;
        }
    }

    // Workgroup reduction of the per-thread record sums.
    wg_partial[local_idx] = thread_sum;
    workgroupBarrier();

    var red_stride: u32 = WG_SIZE / 2u;
    loop {
        if (red_stride == 0u) { break; }
        if (local_idx < red_stride) {
            wg_partial[local_idx] = wg_partial[local_idx] + wg_partial[local_idx + red_stride];
        }
        workgroupBarrier();
        red_stride = red_stride / 2u;
    }

    if (local_idx == 0u) {
        let out_idx = creature_idx * header.num_workgroups_x + wgid.x;
        partials[out_idx] = wg_partial[0];
    }
}
