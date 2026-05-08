//! Multi-creature batched forward + MSE GPU runner (Issue #82).
//!
//! Concatenates per-creature neuron/synapse buffers into a single device-side
//! pair of SSBOs and dispatches one compute pass that scores every
//! `(creature, record)` pair in the chunk. Per-creature MSE partials are
//! reduced inside the shader and summed in `f64` on the host after readback.
//!
//! Compared to the per-creature CPU path (`mse_sum_batch_packed` looped over
//! `loaded.len()` creatures), this trades a CPU-side `N×` arithmetic loop for
//! a single GPU dispatch. The break-even chunk size at N=50 is ≈ 73 records
//! per creature per dispatch (per `docs/gpu-scoring-design.md`); above that
//! the dispatch + transfer overhead is amortised.
//!
//! ## Shader bind layout
//!
//! ```text
//!   binding 0 : uniform Header
//!   binding 1 : storage<read>  records (f32)
//!   binding 2 : storage<read>  neurons (NeuronGpu)
//!   binding 3 : storage<read>  synapses (SynapseGpu)
//!   binding 4 : storage<read>  creatures (CreatureMeta)
//!   binding 5 : storage<read_write> partials (f32, len = num_creatures * num_workgroups_x)
//! ```
//!
//! ## Pipelining lifecycle
//!
//! Two staging slots feed the GPU so chunk `N+1`'s host unpack overlaps chunk
//! `N`'s GPU score. A `crossbeam_channel` of capacity 2 carries
//! "dispatch-complete" handshakes from a poller thread back to the I/O thread
//! so the I/O thread never blocks waiting on `wgpu::Device::poll` — see
//! `score_chunks_pipelined`.

use std::borrow::Cow;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use neat_core::network::CompiledNetwork;
use wgpu::util::DeviceExt;

use crate::gpu::GpuContext;

/// WGSL workgroup size for the batched kernel — must match the shader.
pub const WG_SIZE_X: u32 = 64;

/// Maximum `num_neurons` per creature supported by the shader's private
/// activation scratch. Must mirror `MAX_NEURONS_PER_CREATURE` in the shader.
pub const MAX_NEURONS_PER_CREATURE: u32 = 256;

/// Squash discriminants the kernel inlines. Other types force a CPU fallback.
const SQUASH_IDENTITY: u8 = 0;
const SQUASH_RELU: u8 = 1;
const SQUASH_LOGISTIC: u8 = 6;
const SQUASH_TANH: u8 = 7;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
struct HeaderGpu {
    num_records: u32,
    num_creatures: u32,
    num_inputs: u32,
    num_outputs: u32,
    values_per_record: u32,
    num_workgroups_x: u32,
    _pad0: u32,
    _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct NeuronGpu {
    bias: f32,
    squash_type: u32,
    start_synapse: u32,
    num_synapses: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct SynapseGpu {
    weight: f32,
    from_index: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct CreatureMetaGpu {
    neuron_offset: u32,
    num_non_inputs: u32,
    synapse_offset: u32,
    num_neurons: u32,
}

/// Concatenated per-creature data ready for upload.
#[derive(Debug, Default)]
pub struct BatchedNetworkData {
    pub neurons: Vec<NeuronGpu>,
    pub synapses: Vec<SynapseGpu>,
    pub creatures: Vec<CreatureMetaGpu>,
    pub num_inputs: u32,
    pub num_outputs: u32,
}

/// Errors that prevent the GPU path from running for a given creature set.
/// Callers fall back to the CPU pipeline when they encounter any of these.
#[derive(Debug)]
pub enum GpuPrepareError {
    /// One or more creatures use a squash function the shader does not
    /// implement (anything outside IDENTITY / RELU / LOGISTIC / TANH).
    UnsupportedSquash(u8),
    /// A creature has more than [`MAX_NEURONS_PER_CREATURE`] neurons — would
    /// overflow the shader's private activation array.
    TooManyNeurons {
        creature_idx: usize,
        num_neurons: usize,
    },
    /// Every creature must share the same `(num_inputs, num_outputs)` shape
    /// so the same record format feeds every dispatch.
    MismatchedShape,
}

impl std::fmt::Display for GpuPrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSquash(t) => write!(
                f,
                "GPU forward_mse_batched does not support squash discriminant {t}"
            ),
            Self::TooManyNeurons {
                creature_idx,
                num_neurons,
            } => write!(
                f,
                "creature {creature_idx} has {num_neurons} neurons; GPU shader caps at {MAX_NEURONS_PER_CREATURE}"
            ),
            Self::MismatchedShape => {
                f.write_str("all creatures must share the same (num_inputs, num_outputs) shape")
            }
        }
    }
}

impl std::error::Error for GpuPrepareError {}

fn squash_supported(t: u8) -> bool {
    matches!(
        t,
        SQUASH_IDENTITY | SQUASH_RELU | SQUASH_LOGISTIC | SQUASH_TANH
    )
}

/// Serialise a slice of compiled networks into the flat GPU-side buffers.
///
/// Returns [`GpuPrepareError::UnsupportedSquash`] for any non-supported squash
/// type so the caller can fall back to CPU before allocating any GPU buffers.
pub fn build_batched_network_data(
    networks: &[CompiledNetwork],
    num_inputs: usize,
    num_outputs: usize,
) -> Result<BatchedNetworkData, GpuPrepareError> {
    if networks.is_empty() {
        return Ok(BatchedNetworkData {
            num_inputs: num_inputs as u32,
            num_outputs: num_outputs as u32,
            ..Default::default()
        });
    }

    let mut neurons = Vec::new();
    let mut synapses = Vec::new();
    let mut creatures = Vec::with_capacity(networks.len());

    for (creature_idx, net) in networks.iter().enumerate() {
        if net.num_inputs != num_inputs {
            return Err(GpuPrepareError::MismatchedShape);
        }
        if net.num_neurons > MAX_NEURONS_PER_CREATURE as usize {
            return Err(GpuPrepareError::TooManyNeurons {
                creature_idx,
                num_neurons: net.num_neurons,
            });
        }
        let neuron_offset = neurons.len() as u32;
        let synapse_offset = synapses.len() as u32;
        let num_non_inputs = net.neurons.len() as u32;

        for n in &net.neurons {
            if !squash_supported(n.squash_type) {
                return Err(GpuPrepareError::UnsupportedSquash(n.squash_type));
            }
            neurons.push(NeuronGpu {
                bias: n.bias,
                squash_type: u32::from(n.squash_type),
                start_synapse: n.start_synapse,
                num_synapses: u32::from(n.num_synapses),
            });
        }
        for s in &net.synapses {
            synapses.push(SynapseGpu {
                weight: s.weight,
                from_index: s.from_index,
            });
        }

        creatures.push(CreatureMetaGpu {
            neuron_offset,
            num_non_inputs,
            synapse_offset,
            num_neurons: net.num_neurons as u32,
        });
    }

    Ok(BatchedNetworkData {
        neurons,
        synapses,
        creatures,
        num_inputs: num_inputs as u32,
        num_outputs: num_outputs as u32,
    })
}

/// Reusable GPU pipeline + per-creature buffers for the batched kernel.
///
/// One [`BatchedRunner`] is built once per scoring run. It owns the immutable
/// per-creature SSBOs and the bind-group layout, and lazily grows the records
/// and partials staging buffers as chunks come in.
pub struct BatchedRunner {
    pub ctx: Arc<GpuContext>,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    header_buf: wgpu::Buffer,
    neurons_buf: wgpu::Buffer,
    synapses_buf: wgpu::Buffer,
    creatures_buf: wgpu::Buffer,
    num_creatures: u32,
    num_inputs: u32,
    num_outputs: u32,
    values_per_record: u32,
    /// Records SSBO + readback buffer grow with chunk size; sized for the
    /// largest chunk seen so far.
    records_buf: Option<(wgpu::Buffer, u64)>,
    partials_buf: Option<(wgpu::Buffer, u64)>,
    readback_buf: Option<(wgpu::Buffer, u64)>,
    /// Diagnostic counter — incremented per [`Self::score_chunk`] call.
    pub dispatch_count: usize,
}

impl BatchedRunner {
    /// Construct a runner for the given creature set. Returns
    /// [`GpuPrepareError::UnsupportedSquash`] / [`GpuPrepareError::TooManyNeurons`]
    /// if any creature is incompatible with the shader so the caller can fall
    /// back to CPU.
    pub fn new(
        ctx: Arc<GpuContext>,
        networks: &[CompiledNetwork],
        num_inputs: usize,
        num_outputs: usize,
    ) -> Result<Self, GpuPrepareError> {
        let data = build_batched_network_data(networks, num_inputs, num_outputs)?;
        Ok(Self::from_data(ctx, &data, networks.len() as u32))
    }

    /// Construct a runner from already-serialised batched data. Used by tests
    /// that want to drive the kernel without building a full `CompiledNetwork`
    /// pool.
    pub fn from_data(ctx: Arc<GpuContext>, data: &BatchedNetworkData, num_creatures: u32) -> Self {
        let device = &ctx.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("forward_mse_batched.wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                "../shaders/forward_mse_batched.wgsl"
            ))),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("forward_mse_batched bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("forward_mse_batched pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            ..Default::default()
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("forward_mse_batched"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("forward_mse_batched"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let header = HeaderGpu {
            num_records: 0,
            num_creatures,
            num_inputs: data.num_inputs,
            num_outputs: data.num_outputs,
            values_per_record: data.num_inputs + data.num_outputs,
            num_workgroups_x: 0,
            _pad0: 0,
            _pad1: 0,
        };
        let header_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("forward_mse_batched header"),
            contents: bytemuck::bytes_of(&header),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let neurons_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("forward_mse_batched neurons"),
            contents: bytemuck::cast_slice(&pad_for_storage(&data.neurons)),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let synapses_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("forward_mse_batched synapses"),
            contents: bytemuck::cast_slice(&pad_for_storage(&data.synapses)),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let creatures_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("forward_mse_batched creatures"),
            contents: bytemuck::cast_slice(&pad_for_storage(&data.creatures)),
            usage: wgpu::BufferUsages::STORAGE,
        });

        let values_per_record = data.num_inputs + data.num_outputs;

        Self {
            ctx,
            pipeline,
            bind_group_layout,
            header_buf,
            neurons_buf,
            synapses_buf,
            creatures_buf,
            num_creatures,
            num_inputs: data.num_inputs,
            num_outputs: data.num_outputs,
            values_per_record,
            records_buf: None,
            partials_buf: None,
            readback_buf: None,
            dispatch_count: 0,
        }
    }

    /// Score a single chunk of `n_records` packed records against every
    /// creature. Returns one `f64` MSE-sum partial per creature that the
    /// caller adds to its running total.
    pub fn score_chunk(&mut self, floats: &[f32], n_records: usize) -> Vec<f64> {
        if n_records == 0 || self.num_creatures == 0 {
            return vec![0.0; self.num_creatures as usize];
        }
        let ctx = self.ctx.clone();
        let device = &ctx.device;
        let queue = &ctx.queue;

        let num_workgroups_x = u32::try_from(n_records.div_ceil(WG_SIZE_X as usize))
            .expect("dispatch x exceeds u32::MAX");

        let header = HeaderGpu {
            num_records: u32::try_from(n_records).expect("n_records exceeds u32::MAX"),
            num_creatures: self.num_creatures,
            num_inputs: self.num_inputs,
            num_outputs: self.num_outputs,
            values_per_record: self.values_per_record,
            num_workgroups_x,
            _pad0: 0,
            _pad1: 0,
        };
        queue.write_buffer(&self.header_buf, 0, bytemuck::bytes_of(&header));

        // Records SSBO — grow lazily.
        let records_bytes = std::mem::size_of_val(floats) as u64;
        self.ensure_records_buf(records_bytes);
        let records_buf = self
            .records_buf
            .as_ref()
            .expect("records_buf populated above")
            .0
            .clone();
        queue.write_buffer(&records_buf, 0, bytemuck::cast_slice(floats));

        // Partials SSBO — grow lazily.
        let partials_len =
            self.num_creatures as u64 * num_workgroups_x as u64 * std::mem::size_of::<f32>() as u64;
        self.ensure_partials_buf(partials_len);
        self.ensure_readback_buf(partials_len);
        let partials_buf = self
            .partials_buf
            .as_ref()
            .expect("partials_buf populated above")
            .0
            .clone();
        let readback_buf = self
            .readback_buf
            .as_ref()
            .expect("readback_buf populated above")
            .0
            .clone();

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("forward_mse_batched bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.header_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: records_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.neurons_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.synapses_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.creatures_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: partials_buf.as_entire_binding(),
                },
            ],
        });

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("forward_mse_batched dispatch"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&self.pipeline);
            cpass.set_bind_group(0, &bind_group, &[]);
            cpass.dispatch_workgroups(num_workgroups_x, self.num_creatures, 1);
        }
        encoder.copy_buffer_to_buffer(&partials_buf, 0, &readback_buf, 0, partials_len);
        queue.submit(std::iter::once(encoder.finish()));
        self.dispatch_count += 1;

        // Map and read partials. `wait_for` submitted work to complete.
        let slice = readback_buf.slice(..partials_len);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = sender.send(r);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        receiver
            .recv()
            .expect("partials map_async sender dropped")
            .expect("partials map_async failed");

        let mapped = slice.get_mapped_range();
        let floats: &[f32] = bytemuck::cast_slice(&mapped);

        let mut sums = vec![0.0_f64; self.num_creatures as usize];
        for c in 0..self.num_creatures as usize {
            let start = c * num_workgroups_x as usize;
            let end = start + num_workgroups_x as usize;
            // Sum partials in f64 to keep the running per-chunk error stable.
            let mut s = 0.0_f64;
            for &p in &floats[start..end] {
                s += p as f64;
            }
            sums[c] = s;
        }
        drop(mapped);
        readback_buf.unmap();
        sums
    }

    fn ensure_records_buf(&mut self, bytes: u64) {
        let needed = bytes.max(64);
        let grow = match &self.records_buf {
            Some((_, cap)) => *cap < needed,
            None => true,
        };
        if grow {
            // Round up to a power-of-two-ish to reduce reallocation churn on
            // chunked workloads that grow then plateau.
            let cap = needed.next_power_of_two().max(needed);
            let buf = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("forward_mse_batched records"),
                size: cap,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.records_buf = Some((buf, cap));
        }
    }

    fn ensure_partials_buf(&mut self, bytes: u64) {
        let needed = bytes.max(64);
        let grow = match &self.partials_buf {
            Some((_, cap)) => *cap < needed,
            None => true,
        };
        if grow {
            let cap = needed.next_power_of_two().max(needed);
            let buf = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("forward_mse_batched partials"),
                size: cap,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            self.partials_buf = Some((buf, cap));
        }
    }

    fn ensure_readback_buf(&mut self, bytes: u64) {
        let needed = bytes.max(64);
        let grow = match &self.readback_buf {
            Some((_, cap)) => *cap < needed,
            None => true,
        };
        if grow {
            let cap = needed.next_power_of_two().max(needed);
            let buf = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("forward_mse_batched readback"),
                size: cap,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            self.readback_buf = Some((buf, cap));
        }
    }
}

/// Pad a `Vec<T>` so its byte length is non-zero — wgpu rejects zero-sized
/// `STORAGE` allocations.
fn pad_for_storage<T: Copy + Default + Pod>(v: &[T]) -> Vec<T> {
    if v.is_empty() {
        vec![T::default()]
    } else {
        v.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neat_core::creature::compile_creature;
    use neat_core::creature::parse_creature_json;

    fn synthetic_creature(num_inputs: usize, num_outputs: usize, hidden: usize) -> CompiledNetwork {
        let mut neurons: Vec<String> = Vec::new();
        for h in 0..hidden {
            neurons.push(format!(
                r#"{{"type":"hidden","uuid":"hidden-{h}","bias":0.05,"squash":"TANH"}}"#
            ));
        }
        for o in 0..num_outputs {
            neurons.push(format!(
                r#"{{"type":"output","uuid":"output-{o}","bias":0.0,"squash":"IDENTITY"}}"#
            ));
        }
        let mut synapses: Vec<String> = Vec::new();
        for i in 0..num_inputs {
            for h in 0..hidden {
                let w = 0.05 + 0.001 * ((i * hidden + h) as f64);
                synapses.push(format!(
                    r#"{{"fromUUID":"input-{i}","toUUID":"hidden-{h}","weight":{w}}}"#
                ));
            }
        }
        for h in 0..hidden {
            for o in 0..num_outputs {
                let w = 0.1 + 0.001 * ((h * num_outputs + o) as f64);
                synapses.push(format!(
                    r#"{{"fromUUID":"hidden-{h}","toUUID":"output-{o}","weight":{w}}}"#
                ));
            }
        }
        let json = format!(
            r#"{{"input":{num_inputs},"output":{num_outputs},"forwardOnly":true,"semanticVersion":"4.0.0","neurons":[{}],"synapses":[{}]}}"#,
            neurons.join(","),
            synapses.join(","),
        );
        let creature = parse_creature_json(&json).expect("parse creature");
        compile_creature(&creature).expect("compile")
    }

    #[test]
    fn build_batched_network_data_concatenates_per_creature_offsets() {
        // Two identical 1→1→1 (hidden=1) creatures: each has 1 hidden neuron +
        // 1 output neuron = 2 non-input neurons; 1 input→hidden + 1 hidden→output
        // = 2 synapses.
        let net = synthetic_creature(1, 1, 1);
        let nets = vec![net.clone(), net];

        let data = build_batched_network_data(&nets, 1, 1).expect("supported squash types");
        // Two creatures × (1 hidden + 1 output) = 4 neuron entries total.
        assert_eq!(data.neurons.len(), 4);
        assert_eq!(data.synapses.len(), 4);
        assert_eq!(data.creatures.len(), 2);

        // Second creature's offsets must skip the first creature's data.
        assert_eq!(data.creatures[0].neuron_offset, 0);
        assert_eq!(data.creatures[0].num_non_inputs, 2);
        assert_eq!(data.creatures[1].neuron_offset, 2);
        assert_eq!(data.creatures[1].num_non_inputs, 2);

        assert_eq!(data.creatures[0].synapse_offset, 0);
        assert_eq!(data.creatures[1].synapse_offset, 2);

        assert_eq!(data.num_inputs, 1);
        assert_eq!(data.num_outputs, 1);
    }

    #[test]
    fn build_batched_network_data_rejects_unsupported_squash() {
        // Build a creature with an unsupported aggregate squash (MEAN = 37).
        // We can't easily construct one through the public JSON path because
        // `parse_squash_name` may reject — so build a fake CompiledNetwork by
        // mutating squash_type after compilation.
        let mut net = synthetic_creature(1, 1, 1);
        if let Some(n) = net.neurons.first_mut() {
            n.squash_type = 32; // MINIMUM — not supported by the shader.
        }
        let err =
            build_batched_network_data(&[net], 1, 1).expect_err("unsupported squash rejected");
        match err {
            GpuPrepareError::UnsupportedSquash(t) => assert_eq!(t, 32),
            other => panic!("expected UnsupportedSquash, got {other:?}"),
        }
    }

    #[test]
    fn build_batched_network_data_rejects_too_many_neurons() {
        let mut net = synthetic_creature(1, 1, 1);
        net.num_neurons = (MAX_NEURONS_PER_CREATURE as usize) + 1;
        let err = build_batched_network_data(&[net], 1, 1).expect_err("oversized network rejected");
        assert!(matches!(err, GpuPrepareError::TooManyNeurons { .. }));
    }

    #[test]
    fn build_batched_network_data_rejects_shape_mismatch() {
        let net1 = synthetic_creature(2, 1, 1);
        let net2 = synthetic_creature(3, 1, 1);
        let err = build_batched_network_data(&[net1, net2], 2, 1)
            .expect_err("input shape mismatch rejected");
        assert!(matches!(err, GpuPrepareError::MismatchedShape));
    }
}
