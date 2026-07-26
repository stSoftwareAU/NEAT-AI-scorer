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

use crate::cost::CostKind;
use crate::gpu::GpuContext;

/// WGSL workgroup size for the batched kernel — must match the shader.
pub const WG_SIZE_X: u32 = 64;

/// Largest `num_neurons` per creature the fast `forward_mse_batched` kernel can
/// host in its fixed-size `private` activation array. Must mirror
/// `MAX_NEURONS_PER_CREATURE` in `forward_mse_batched.wgsl`.
///
/// Creatures at or below this stay on the original single-dispatch kernel
/// (fastest for small creatures — Issue #82). Creatures above it route to the
/// `forward_mse_scratch` kernel (Issue #182), which holds activations in a
/// runtime-sized `storage` buffer and so has no compile-time neuron cap.
pub const MAX_NEURONS_PER_CREATURE: u32 = 256;

/// Largest **non-input** neuron count for a scratch-kernel creature to count as
/// *shallow* (Issue #467).
///
/// Inputs count towards `num_neurons`, so a creature with thousands of inputs
/// and a handful of hidden neurons still exceeds [`MAX_NEURONS_PER_CREATURE`]
/// and routes to `forward_mse_scratch`. Those shallow scratch pools behave
/// nothing like the deep production shape #317 measured: the M4 Pro A/B in
/// `docs/performance-baseline.md` (Issue #467) has GPU ~29 % faster than CPU on
/// the 2461-input / 19-hidden Enceladus creature, so `--gpu auto` routes them to
/// GPU. The bound reuses the private-kernel cap because the same 256-neuron
/// figure was validated at the boundary in that sweep.
pub const MAX_SHALLOW_NON_INPUT_NEURONS: u32 = 256;

/// Absolute sanity ceiling on per-creature neuron count (Issue #182). The
/// scratch kernel has no architectural cap, but a corrupt creature claiming an
/// absurd neuron count would demand a nonsensical scratch allocation — reject
/// it up-front instead. One million neurons is far above any real evolved
/// creature (observed maxima are in the low thousands).
pub const MAX_NEURONS_ABSOLUTE: u32 = 1 << 20;

/// Default activation-scratch memory budget for the `forward_mse_scratch`
/// kernel, in bytes (512 MiB). The host caps the grid-stride thread count so
/// `num_creatures * G_x * WG_SIZE * max_neurons * 4` bytes stays within this
/// budget. Override with `NEAT_SCORER_GPU_SCRATCH_BYTES` (used by benchmarks).
pub const DEFAULT_SCRATCH_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

/// Highest point-wise squash discriminant the kernels inline (Issue #305).
///
/// Discriminants `0..=31` are the element-wise activations
/// (`neat_core::squash::SquashType::Identity` through `Isru`) — the shader's
/// `activate()` implements every one, matching `apply_squash` + the
/// `apply_limit_range` clamp.
const MAX_POINTWISE_SQUASH: u8 = 31;

/// Highest squash discriminant the kernels host, inclusive of the aggregate
/// reductions added by Issue #312.
///
/// `32..=34` are the aggregate squashes MINIMUM/MAXIMUM/IF: they combine the
/// individual weighted inputs (min / max / synapse-type branch) rather than
/// their sum, and the kernels now reduce them inline to match
/// `neat_core::batch_scoring::neuron_activation_scalar`. The remaining
/// aggregates HYPOT/HYPOTv2/MEAN (`35..=37`) are not yet hosted and still force
/// a CPU fallback.
///
/// Derived from [`MAX_POINTWISE_SQUASH`] plus the three inline aggregate
/// reductions (MINIMUM/MAXIMUM/IF) so the boundary stays self-documenting.
const MAX_SUPPORTED_SQUASH: u8 = MAX_POINTWISE_SQUASH + 3;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
struct HeaderGpu {
    num_records: u32,
    num_creatures: u32,
    num_inputs: u32,
    num_outputs: u32,
    values_per_record: u32,
    num_workgroups_x: u32,
    // Scratch kernel reads its activation stride here (`Header.max_neurons`);
    // the private kernel ignores it. See [`BatchedRunner::score_chunk`].
    _pad0: u32,
    // Per-record error mode (Issue #316): `CostKind::gpu_error_code` — 0 for
    // MSE/RMSE (squared error), 1 for MAE (absolute error). Both kernels read
    // this from the shader `Header.cost_kind` field.
    cost_kind: u32,
}

/// GPU-side mirror of a compiled neuron (`#[repr(C)]`, uploaded as an SSBO element).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct NeuronGpu {
    bias: f32,
    squash_type: u32,
    start_synapse: u32,
    num_synapses: u32,
    /// Non-zero for a constant neuron: the kernel returns a clamped bias and
    /// ignores this neuron's synapses, matching the CPU `is_constant` branch
    /// (Issue #312). Adds a 4th `u32` — the std430 array stride stays sound
    /// (all-scalar struct, 20-byte stride, no interior padding).
    is_constant: u32,
}

/// GPU-side mirror of a compiled synapse (`#[repr(C)]`, uploaded as an SSBO element).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct SynapseGpu {
    weight: f32,
    from_index: u32,
    /// `neat_core::synapse_type::SynapseType` discriminant, consumed by the IF
    /// aggregate to bucket inputs into condition/positive/negative (Issue #312).
    /// The 12-byte all-scalar struct keeps the std430 stride sound.
    synapse_type: u32,
}

/// GPU-side mirror of a compiled creature's buffer layout (`#[repr(C)]`, uploaded as an SSBO element).
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
    /// All creatures' neurons concatenated into one upload buffer.
    pub neurons: Vec<NeuronGpu>,
    /// All creatures' synapses concatenated into one upload buffer.
    pub synapses: Vec<SynapseGpu>,
    /// Per-creature metadata indexing into `neurons`/`synapses`.
    pub creatures: Vec<CreatureMetaGpu>,
    /// Input count shared by every creature in the batch.
    pub num_inputs: u32,
    /// Output count shared by every creature in the batch.
    pub num_outputs: u32,
}

/// Errors that prevent the GPU path from running for a given creature set.
/// Callers fall back to the CPU pipeline when they encounter any of these.
#[derive(Debug)]
pub enum GpuPrepareError {
    /// One or more creatures use a squash function the shader does not
    /// implement. Since Issue #305 the shader inlines every point-wise
    /// activation (0..=31), and Issue #312 added the aggregate reductions
    /// MINIMUM/MAXIMUM/IF (32..=34); this now fires only for the remaining
    /// aggregates HYPOT/HYPOTv2/MEAN (35..=37).
    UnsupportedSquash(u8),
    /// A creature claims more than [`MAX_NEURONS_ABSOLUTE`] neurons — far
    /// beyond any real evolved creature, so it is rejected rather than sized
    /// into a nonsensical scratch allocation (Issue #182). Creatures merely
    /// above the 256-neuron private-array cap are *not* rejected: they route to
    /// the `forward_mse_scratch` kernel.
    TooManyNeurons {
        /// Index of the offending creature within the batch.
        creature_idx: usize,
        /// The rejected neuron count that exceeded [`MAX_NEURONS_ABSOLUTE`].
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
                "creature {creature_idx} claims {num_neurons} neurons; GPU runner caps at {MAX_NEURONS_ABSOLUTE}"
            ),
            Self::MismatchedShape => {
                f.write_str("all creatures must share the same (num_inputs, num_outputs) shape")
            }
        }
    }
}

impl std::error::Error for GpuPrepareError {}

fn squash_supported(t: u8) -> bool {
    // Every point-wise activation (0..=31) is inlined by the shader's
    // `activate()` (Issue #305); the aggregate squashes MINIMUM/MAXIMUM/IF
    // (32..=34) are reduced inline (Issue #312). HYPOT/HYPOTv2/MEAN (35..=37)
    // are still unhosted and force a CPU fallback.
    t <= MAX_SUPPORTED_SQUASH
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
        // Issue #182: creatures above the 256 private-array cap are no longer
        // rejected — they run on the `forward_mse_scratch` kernel. Only an
        // absurd, almost-certainly-corrupt neuron count is refused.
        if net.num_neurons > MAX_NEURONS_ABSOLUTE as usize {
            return Err(GpuPrepareError::TooManyNeurons {
                creature_idx,
                num_neurons: net.num_neurons,
            });
        }
        let neuron_offset = neurons.len() as u32;
        let synapse_offset = synapses.len() as u32;
        let num_non_inputs = net.neurons.len() as u32;

        for n in &net.neurons {
            // Constant neurons are hosted since Issue #312 (clamped bias,
            // synapses ignored), so they no longer force a CPU fallback. A
            // constant neuron's squash discriminant is irrelevant on the GPU —
            // the `is_constant` flag short-circuits before the squash branch —
            // so skip the squash-support check for it.
            if !n.is_constant && !squash_supported(n.squash_type) {
                return Err(GpuPrepareError::UnsupportedSquash(n.squash_type));
            }
            neurons.push(NeuronGpu {
                bias: n.bias,
                squash_type: u32::from(n.squash_type),
                start_synapse: n.start_synapse,
                num_synapses: u32::from(n.num_synapses),
                is_constant: u32::from(n.is_constant),
            });
        }
        for s in &net.synapses {
            synapses.push(SynapseGpu {
                weight: s.weight,
                // neat-core #177 narrowed `SynapseData::from_index` to `u16`;
                // the GPU/WGSL layout keeps `from_index` as `u32`, so widen here.
                from_index: u32::from(s.from_index),
                // Widen the `SynapseType` discriminant for the IF aggregate
                // (Issue #312).
                synapse_type: u32::from(s.synapse_type),
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

/// Which compute kernel a [`BatchedRunner`] drives, chosen at construction by
/// the largest per-creature neuron count (Issue #182).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelKind {
    /// `forward_mse_batched` — fixed `private` activation array, one thread per
    /// `(creature, record)` pair, single dispatch. Fastest for creatures at or
    /// below [`MAX_NEURONS_PER_CREATURE`] (Issue #82).
    Private,
    /// `forward_mse_scratch` — runtime-sized `storage` activation scratch with
    /// a bounded grid-stride loop. Hosts creatures of any size above the
    /// private-array cap (Issue #182).
    Scratch,
}

/// Identity of the buffers a bind group references (Issue #322).
///
/// A `wgpu::BindGroup` stays valid for reuse across dispatches as long as every
/// buffer it binds is the same allocation and any explicitly-sized binding keeps
/// the same size. The immutable per-creature SSBOs (`header`, `neurons`,
/// `synapses`, `creatures`) never change, so only three things can invalidate a
/// cached bind group: the growable `records`/`partials`/`scratch` buffers being
/// reallocated, or the scratch binding being resized. A monotonic generation
/// counter per growable buffer (bumped on each reallocation) plus the bound
/// scratch size captures exactly that — so a matching signature means the cached
/// bind group is still correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BindGroupSig {
    records_gen: u64,
    partials_gen: u64,
    scratch_gen: u64,
    scratch_bytes: u64,
}

/// Whether a cached bind group with signature `cached` must be rebuilt to serve
/// a dispatch whose current buffer signature is `current` (Issue #322).
///
/// Pure so the reuse decision is unit-testable on CPU-only CI without a live GPU.
fn bind_group_needs_rebuild(cached: Option<BindGroupSig>, current: BindGroupSig) -> bool {
    cached != Some(current)
}

/// Reusable GPU pipeline + per-creature buffers for the batched kernel.
///
/// One [`BatchedRunner`] is built once per scoring run. It owns the immutable
/// per-creature SSBOs and the bind-group layout, and lazily grows the records
/// and partials staging buffers as chunks come in.
pub struct BatchedRunner {
    /// Shared GPU context (device + queue) this runner submits work to.
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
    /// Per-record error mode written into the kernel header each dispatch
    /// (Issue #316): `CostKind::gpu_error_code` — 0 for MSE/RMSE (squared
    /// error), 1 for MAE (absolute error).
    cost_code: u32,
    /// Which kernel this runner drives (Issue #182).
    kernel: KernelKind,
    /// Largest per-creature neuron count — the activation scratch stride for
    /// the [`KernelKind::Scratch`] path.
    max_neurons: u32,
    /// Scratch-kernel activation memory budget in bytes (Issue #182).
    scratch_budget_bytes: u64,
    /// Records SSBO + readback buffer grow with chunk size; sized for the
    /// largest chunk seen so far.
    records_buf: Option<(wgpu::Buffer, u64)>,
    partials_buf: Option<(wgpu::Buffer, u64)>,
    readback_buf: Option<(wgpu::Buffer, u64)>,
    /// Activation scratch SSBO for [`KernelKind::Scratch`]; grows lazily.
    scratch_buf: Option<(wgpu::Buffer, u64)>,
    /// Cached bind group reused across dispatches while its buffers are
    /// unchanged (Issue #322). Rebuilt only when a growable buffer is
    /// reallocated or the scratch binding is resized.
    bind_group: Option<wgpu::BindGroup>,
    /// Signature of the buffers `bind_group` references, for reuse validation.
    bind_group_sig: Option<BindGroupSig>,
    /// Reallocation generation counters for the growable buffers — bumped each
    /// time the corresponding buffer is grown so a cached bind group referencing
    /// the old allocation is invalidated (Issue #322).
    records_gen: u64,
    partials_gen: u64,
    scratch_gen: u64,
    /// Diagnostic counter — incremented per [`Self::score_chunk`] call.
    pub dispatch_count: usize,
    /// Diagnostic counter — incremented each time the bind group is (re)built
    /// rather than reused (Issue #322). Reused for the bind-group-reuse test.
    pub bind_group_builds: usize,
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
        cost: CostKind,
    ) -> Result<Self, GpuPrepareError> {
        let data = build_batched_network_data(networks, num_inputs, num_outputs)?;
        Ok(Self::from_data(ctx, &data, networks.len() as u32, cost))
    }

    /// Construct a runner from already-serialised batched data, for callers
    /// that want to drive the kernel without building a full `CompiledNetwork`
    /// pool. Issue #470 audit: the only in-tree caller today is
    /// [`BatchedRunner::new`]; `num_creatures` stays explicit because it is the
    /// dispatch height and every other field of the runner is derived from it.
    ///
    /// `cost` selects the per-record loss the kernel accumulates (Issue #316):
    /// squared error for MSE/RMSE, absolute error for MAE. Only a
    /// [`CostKind::gpu_supported`] cost should reach here.
    pub fn from_data(
        ctx: Arc<GpuContext>,
        data: &BatchedNetworkData,
        num_creatures: u32,
        cost: CostKind,
    ) -> Self {
        let device = &ctx.device;

        // Issue #182 — route by the largest per-creature neuron count: the
        // fast private-array kernel for small creatures, the storage-scratch
        // kernel for anything above the 256 cap.
        let max_neurons = data
            .creatures
            .iter()
            .map(|c| c.num_neurons)
            .max()
            .unwrap_or(0);
        let kernel = if max_neurons > MAX_NEURONS_PER_CREATURE {
            KernelKind::Scratch
        } else {
            KernelKind::Private
        };
        // The scratch buffer is bound in a single binding, so its budget can
        // never exceed the device's max storage-buffer binding size (Issue
        // #182). Cap it so an over-large `NEAT_SCORER_GPU_SCRATCH_BYTES` (or the
        // 512 MiB default on a device with a smaller limit) cannot create an
        // unbindable buffer that silently yields NaN partials.
        let binding_limit = device.limits().max_storage_buffer_binding_size;
        let scratch_budget_bytes = scratch_budget_bytes_from_env().min(binding_limit.max(64));

        let (shader_src, shader_label, entry_point): (&str, &str, &str) = match kernel {
            KernelKind::Private => (
                include_str!("../shaders/forward_mse_batched.wgsl"),
                "forward_mse_batched.wgsl",
                "forward_mse_batched",
            ),
            KernelKind::Scratch => (
                include_str!("../shaders/forward_mse_scratch.wgsl"),
                "forward_mse_scratch.wgsl",
                "forward_mse_scratch",
            ),
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(shader_label),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(shader_src)),
        });

        // The scratch kernel adds binding 6 (read_write activation scratch).
        let mut layout_entries = vec![
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
        ];
        if kernel == KernelKind::Scratch {
            layout_entries.push(wgpu::BindGroupLayoutEntry {
                binding: 6,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            });
        }
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("forward_mse bind group layout"),
            entries: &layout_entries,
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("forward_mse pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            ..Default::default()
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(shader_label),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some(entry_point),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let cost_code = cost.gpu_error_code();
        let header = HeaderGpu {
            num_records: 0,
            num_creatures,
            num_inputs: data.num_inputs,
            num_outputs: data.num_outputs,
            values_per_record: data.num_inputs + data.num_outputs,
            num_workgroups_x: 0,
            _pad0: 0,
            cost_kind: cost_code,
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
            cost_code,
            kernel,
            max_neurons,
            scratch_budget_bytes,
            records_buf: None,
            partials_buf: None,
            readback_buf: None,
            scratch_buf: None,
            bind_group: None,
            bind_group_sig: None,
            records_gen: 0,
            partials_gen: 0,
            scratch_gen: 0,
            dispatch_count: 0,
            bind_group_builds: 0,
        }
    }

    /// Which kernel this runner drives (Issue #182).
    #[allow(dead_code)] // consumed by the GPU parity integration test.
    pub fn kernel(&self) -> KernelKind {
        self.kernel
    }

    /// Score a single chunk of `n_records` packed records against every
    /// creature. Returns one `f64` MSE-sum partial per creature that the
    /// caller adds to its running total.
    ///
    /// Returns `Err` when the GPU readback (`map_async`) fails — a recoverable
    /// device hiccup such as device loss, an out-of-memory condition, or a
    /// validation error during readback — so the `--gpu auto` caller can fall
    /// back to the CPU instead of panicking mid-run (Issue #273).
    pub fn score_chunk(&mut self, floats: &[f32], n_records: usize) -> Result<Vec<f64>, String> {
        if n_records == 0 || self.num_creatures == 0 {
            return Ok(vec![0.0; self.num_creatures as usize]);
        }
        let ctx = self.ctx.clone();
        let device = &ctx.device;
        let queue = &ctx.queue;

        // `num_workgroups_x` is the records-covering grid for the private
        // kernel (one thread per record); for the scratch kernel it is the
        // bounded grid-stride width `G_x` chosen against the memory budget so
        // the activation scratch stays small (Issue #182).
        let num_workgroups_x = match self.kernel {
            KernelKind::Private => u32::try_from(n_records.div_ceil(WG_SIZE_X as usize))
                .expect("dispatch x exceeds u32::MAX"),
            KernelKind::Scratch => self.scratch_workgroups_x(n_records),
        };

        // The scratch kernel reads `max_neurons` from the `_pad0` header slot
        // (the WGSL `Header.max_neurons` field); the private kernel ignores it.
        let max_neurons_field = match self.kernel {
            KernelKind::Private => 0,
            KernelKind::Scratch => self.max_neurons,
        };

        let header = HeaderGpu {
            num_records: u32::try_from(n_records).expect("n_records exceeds u32::MAX"),
            num_creatures: self.num_creatures,
            num_inputs: self.num_inputs,
            num_outputs: self.num_outputs,
            values_per_record: self.values_per_record,
            num_workgroups_x,
            _pad0: max_neurons_field,
            // Issue #316: select squared (MSE/RMSE) vs absolute (MAE) error.
            cost_kind: self.cost_code,
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

        // Activation scratch SSBO for the scratch kernel — grows lazily. The
        // buffer is rounded up for reuse, but only the exact bytes this dispatch
        // needs are bound so the binding never exceeds the device limit.
        let scratch_binding = if self.kernel == KernelKind::Scratch {
            let scratch_floats = self.num_creatures as u64
                * num_workgroups_x as u64
                * WG_SIZE_X as u64
                * self.max_neurons as u64;
            let scratch_bytes = (scratch_floats * std::mem::size_of::<f32>() as u64).max(64);
            self.ensure_scratch_buf(scratch_bytes);
            let buf = self
                .scratch_buf
                .as_ref()
                .expect("scratch_buf populated above")
                .0
                .clone();
            Some((buf, scratch_bytes))
        } else {
            None
        };

        // Issue #322 — reuse the bind group across dispatches. It only becomes
        // stale when a growable buffer is reallocated (tracked by the `*_gen`
        // counters) or the scratch binding is resized, so rebuild solely on a
        // signature change. This drops one `create_bind_group` + entries `Vec`
        // allocation off every steady-state dispatch.
        let scratch_bytes_bound = scratch_binding.as_ref().map(|(_, b)| *b).unwrap_or(0);
        let sig = BindGroupSig {
            records_gen: self.records_gen,
            partials_gen: self.partials_gen,
            scratch_gen: self.scratch_gen,
            scratch_bytes: scratch_bytes_bound,
        };
        if bind_group_needs_rebuild(self.bind_group_sig, sig) {
            let mut bind_entries = vec![
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
            ];
            if let Some((scratch_buf, scratch_bytes)) = scratch_binding.as_ref() {
                bind_entries.push(wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: scratch_buf,
                        offset: 0,
                        size: Some(
                            std::num::NonZeroU64::new(*scratch_bytes).expect("scratch >= 64"),
                        ),
                    }),
                });
            }
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("forward_mse bind group"),
                layout: &self.bind_group_layout,
                entries: &bind_entries,
            });
            self.bind_group = Some(bind_group);
            self.bind_group_sig = Some(sig);
            self.bind_group_builds += 1;
        }
        let bind_group = self
            .bind_group
            .as_ref()
            .expect("bind group built above when the signature changed");

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("forward_mse dispatch"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&self.pipeline);
            cpass.set_bind_group(0, bind_group, &[]);
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
        map_readback_result(receiver.recv())?;

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
        Ok(sums)
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
            // A new allocation invalidates any cached bind group (Issue #322).
            self.records_gen += 1;
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
            // A new allocation invalidates any cached bind group (Issue #322).
            self.partials_gen += 1;
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

    fn ensure_scratch_buf(&mut self, bytes: u64) {
        let needed = bytes.max(64);
        let grow = match &self.scratch_buf {
            Some((_, cap)) => *cap < needed,
            None => true,
        };
        if grow {
            let cap = needed.next_power_of_two().max(needed);
            let buf = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("forward_mse_scratch activations"),
                size: cap,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            self.scratch_buf = Some((buf, cap));
            // A new allocation invalidates any cached bind group (Issue #322).
            self.scratch_gen += 1;
        }
    }

    /// Bounded grid-stride width `G_x` for the scratch kernel: enough
    /// workgroups to cover the records once, capped so the activation scratch
    /// (`num_creatures * G_x * WG_SIZE * max_neurons` floats) fits the memory
    /// budget. Always at least one workgroup.
    fn scratch_workgroups_x(&self, n_records: usize) -> u32 {
        scratch_workgroups_x_for(
            n_records,
            self.num_creatures,
            self.max_neurons,
            self.scratch_budget_bytes,
        )
    }
}

/// Topology of a directory-mode creature pool for kernel routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryGpuTopology {
    /// Every creature fits the fast private-array kernel.
    AllPrivate,
    /// At least one creature exceeds [`MAX_NEURONS_PER_CREATURE`] and none fit
    /// the private kernel alone.
    ScratchOnly,
    /// Both private-fit and scratch-required creatures — dual-kernel path.
    Mixed,
}

/// Clamp requested pipelined in-flight dispatches for directory-mode GPU scoring.
///
/// `inflight_chunks == 2` overlaps host unpack with GPU readback on a worker
/// thread. For scratch/mixed pools (large SSBO activations, production-scale
/// `32 MiB` reads), that combination has been observed to **SIGSEGV on Metal**
/// when a streamed chunk spans a `.bin` file boundary (Issue #319). Keep those
/// pools on the synchronous path (`1`); all-private synthetic benches may still
/// request `2`.
pub fn effective_directory_gpu_inflight(topology: DirectoryGpuTopology, requested: usize) -> usize {
    match topology {
        DirectoryGpuTopology::AllPrivate => requested,
        DirectoryGpuTopology::ScratchOnly | DirectoryGpuTopology::Mixed => 1,
    }
}

/// Classify a compiled creature pool for GPU kernel selection.
pub fn directory_gpu_topology(networks: &[CompiledNetwork]) -> DirectoryGpuTopology {
    let mut has_private = false;
    let mut has_scratch = false;
    for net in networks {
        if u32::try_from(net.num_neurons).unwrap_or(u32::MAX) > MAX_NEURONS_PER_CREATURE {
            has_scratch = true;
        } else {
            has_private = true;
        }
    }
    match (has_private, has_scratch) {
        (true, true) => DirectoryGpuTopology::Mixed,
        (false, true) => DirectoryGpuTopology::ScratchOnly,
        (true, false) | (false, false) => DirectoryGpuTopology::AllPrivate,
    }
}

/// Whether every creature in the pool is **shallow** — Issue #467.
///
/// "Shallow" means the non-input neuron count (`num_neurons - num_inputs`) is
/// within [`MAX_SHALLOW_NON_INPUT_NEURONS`]. Such creatures still exceed the
/// private-kernel cap once their inputs are counted, so they run on
/// `forward_mse_scratch`, but they carry far less per-record kernel work than
/// the deep production shape and beat CPU on Apple Silicon (Issue #467).
///
/// An empty pool is **not** shallow: there is no evidence to justify GPU, and
/// the caller must not read "no creatures" as "GPU is faster".
pub fn directory_pool_is_shallow(networks: &[CompiledNetwork]) -> bool {
    !networks.is_empty()
        && networks.iter().all(|net| {
            let non_input = net.num_neurons.saturating_sub(net.num_inputs);
            u32::try_from(non_input).unwrap_or(u32::MAX) <= MAX_SHALLOW_NON_INPUT_NEURONS
        })
}

/// Directory-mode GPU runners — may hold private and/or scratch kernels when the
/// creature pool is mixed so small creatures avoid the scratch-kernel tax.
pub struct DirectoryGpuRunners {
    private: Option<BatchedRunner>,
    private_orig: Vec<usize>,
    scratch: Option<BatchedRunner>,
    scratch_orig: Vec<usize>,
    creature_count: usize,
}

impl DirectoryGpuRunners {
    /// Build zero, one, or two runners from a heterogeneous creature pool.
    pub fn new(
        ctx: Arc<GpuContext>,
        networks: &[CompiledNetwork],
        num_inputs: usize,
        num_outputs: usize,
        cost: CostKind,
    ) -> Result<Self, GpuPrepareError> {
        let creature_count = networks.len();
        let mut private_nets: Vec<CompiledNetwork> = Vec::new();
        let mut private_orig: Vec<usize> = Vec::new();
        let mut scratch_nets: Vec<CompiledNetwork> = Vec::new();
        let mut scratch_orig: Vec<usize> = Vec::new();

        for (i, net) in networks.iter().enumerate() {
            if u32::try_from(net.num_neurons).unwrap_or(u32::MAX) > MAX_NEURONS_PER_CREATURE {
                scratch_orig.push(i);
                scratch_nets.push(net.clone());
            } else {
                private_orig.push(i);
                private_nets.push(net.clone());
            }
        }

        let private = if private_orig.is_empty() {
            None
        } else {
            Some(BatchedRunner::new(
                ctx.clone(),
                &private_nets,
                num_inputs,
                num_outputs,
                cost,
            )?)
        };
        let scratch = if scratch_orig.is_empty() {
            None
        } else {
            Some(BatchedRunner::new(
                ctx,
                &scratch_nets,
                num_inputs,
                num_outputs,
                cost,
            )?)
        };

        Ok(Self {
            private,
            private_orig,
            scratch,
            scratch_orig,
            creature_count,
        })
    }

    /// Score one packed chunk; returns one MSE-sum partial per original creature.
    pub fn score_chunk(&mut self, floats: &[f32], n_records: usize) -> Result<Vec<f64>, String> {
        let mut totals = vec![0.0_f64; self.creature_count];
        if let Some(ref mut runner) = self.private {
            let partial = runner.score_chunk(floats, n_records)?;
            for (slot, &orig) in self.private_orig.iter().enumerate() {
                totals[orig] += partial[slot];
            }
        }
        if let Some(ref mut runner) = self.scratch {
            let partial = runner.score_chunk(floats, n_records)?;
            for (slot, &orig) in self.scratch_orig.iter().enumerate() {
                totals[orig] += partial[slot];
            }
        }
        Ok(totals)
    }

    /// Combined dispatch count across all active runners.
    pub fn dispatch_count(&self) -> usize {
        self.private.as_ref().map(|r| r.dispatch_count).unwrap_or(0)
            + self.scratch.as_ref().map(|r| r.dispatch_count).unwrap_or(0)
    }

    /// Stable label for the `gpuKernel` JSON field.
    pub fn kernel_label(&self) -> String {
        match self.topology() {
            DirectoryGpuTopology::Mixed => "forward_mse_batched+forward_mse_scratch".to_string(),
            DirectoryGpuTopology::ScratchOnly => "forward_mse_scratch".to_string(),
            DirectoryGpuTopology::AllPrivate => "forward_mse_batched".to_string(),
        }
    }

    /// Topology of this runner set.
    pub fn topology(&self) -> DirectoryGpuTopology {
        match (&self.private, &self.scratch) {
            (Some(_), Some(_)) => DirectoryGpuTopology::Mixed,
            (None, Some(_)) => DirectoryGpuTopology::ScratchOnly,
            (Some(_), None) | (None, None) => DirectoryGpuTopology::AllPrivate,
        }
    }
}

/// Pure helper for `BatchedRunner::scratch_workgroups_x` so the bounding
/// logic is unit-testable without a GPU (Issue #182).
pub fn scratch_workgroups_x_for(
    n_records: usize,
    num_creatures: u32,
    max_neurons: u32,
    budget_bytes: u64,
) -> u32 {
    if n_records == 0 || num_creatures == 0 || max_neurons == 0 {
        return 1;
    }
    // Workgroups needed to cover every record exactly once.
    let needed = (n_records as u64).div_ceil(WG_SIZE_X as u64).max(1);
    // Scratch bytes consumed per unit of G_x.
    let per_gx_bytes = num_creatures as u64
        * WG_SIZE_X as u64
        * max_neurons as u64
        * std::mem::size_of::<f32>() as u64;
    let budget_gx = (budget_bytes / per_gx_bytes.max(1)).max(1);
    let gx = needed.min(budget_gx).max(1);
    u32::try_from(gx).unwrap_or(u32::MAX)
}

/// Resolve the scratch-kernel memory budget from `NEAT_SCORER_GPU_SCRATCH_BYTES`,
/// falling back to [`DEFAULT_SCRATCH_BUDGET_BYTES`] (Issue #182).
fn scratch_budget_bytes_from_env() -> u64 {
    let env = std::env::var("NEAT_SCORER_GPU_SCRATCH_BYTES").ok();
    let (parsed, warning) = crate::env_tuning::parse_tuning_var(
        "NEAT_SCORER_GPU_SCRATCH_BYTES",
        env.as_deref(),
        DEFAULT_SCRATCH_BUDGET_BYTES,
        // A zero budget is semantically invalid, so treat it like a parse
        // failure (warn + default) rather than a silent fallback.
        |s| s.parse::<u64>().ok().filter(|&b| b > 0),
    );
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
    parsed
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

/// Turn the readback channel result into `Ok(())` or a descriptive error
/// string (Issue #273).
///
/// The GPU readback maps `partials` via `map_async` and reports completion on a
/// one-shot channel. Two failure modes are folded into recoverable `Err`s
/// rather than panicking: the callback sender being dropped (`RecvError`) and
/// the map itself failing (device loss, out-of-memory, or a validation error
/// during readback). Both are external-I/O faults on the GPU device, so
/// surfacing them as `Err` lets the `--gpu auto` caller fall back to the CPU.
///
/// Generic over the map error type so the mapping is unit-testable without a
/// live GPU (`wgpu::BufferAsyncError` has no public constructor).
fn map_readback_result<E: std::fmt::Debug>(
    recv: Result<Result<(), E>, std::sync::mpsc::RecvError>,
) -> Result<(), String> {
    recv.map_err(|_| "partials map_async sender dropped".to_string())?
        .map_err(|e| format!("partials map_async failed: {e:?}"))?;
    Ok(())
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
    fn build_batched_network_data_widens_from_index_to_u32() {
        // Regression for #250: neat-core #177 narrowed `SynapseData::from_index`
        // to `u16`, while the GPU/WGSL `SynapseGpu` layout keeps it as `u32`.
        // `build_batched_network_data` must widen losslessly at the upload
        // boundary, so every `SynapseGpu.from_index` equals the `u32` widening
        // of the source network's `u16` `from_index`.
        let net = synthetic_creature(2, 1, 2);
        let expected: Vec<u32> = net
            .synapses
            .iter()
            .map(|s| u32::from(s.from_index))
            .collect();

        let data = build_batched_network_data(&[net], 2, 1).expect("supported squash types");

        let actual: Vec<u32> = data.synapses.iter().map(|s| s.from_index).collect();
        assert_eq!(
            actual, expected,
            "from_index must be widened u16 -> u32 unchanged"
        );
    }

    #[test]
    fn build_batched_network_data_rejects_unsupported_squash() {
        // Build a creature with a still-unhosted aggregate squash (MEAN = 37).
        // MINIMUM/MAXIMUM/IF (32..=34) are hosted since Issue #312, so pick one
        // of the remaining aggregates to exercise the rejection path.
        let mut net = synthetic_creature(1, 1, 1);
        if let Some(n) = net.neurons.first_mut() {
            n.squash_type = 37; // MEAN — not yet hosted by the shader.
        }
        let err =
            build_batched_network_data(&[net], 1, 1).expect_err("unsupported squash rejected");
        match err {
            GpuPrepareError::UnsupportedSquash(t) => assert_eq!(t, 37),
            other => panic!("expected UnsupportedSquash, got {other:?}"),
        }
    }

    /// Issue #305: every point-wise squash discriminant (0..=31) is now inlined
    /// by the shader, so `build_batched_network_data` accepts a creature using
    /// any of them without falling back to CPU.
    #[test]
    fn build_batched_network_data_accepts_all_pointwise_squashes() {
        for t in 0u8..=MAX_POINTWISE_SQUASH {
            let mut net = synthetic_creature(1, 1, 1);
            // Set the hidden neuron's squash to the discriminant under test.
            if let Some(n) = net.neurons.first_mut() {
                n.squash_type = t;
            }
            build_batched_network_data(&[net], 1, 1)
                .unwrap_or_else(|e| panic!("point-wise squash {t} must be GPU-supported: {e:?}"));
        }
    }

    /// Issue #312: the aggregate squashes MINIMUM/MAXIMUM/IF (32..=34) are now
    /// reduced inline by the kernels, so `build_batched_network_data` accepts a
    /// creature using any of them and serialises its discriminant unchanged.
    #[test]
    fn build_batched_network_data_accepts_min_max_if_aggregates() {
        for t in 32u8..=34u8 {
            let mut net = synthetic_creature(1, 1, 1);
            if let Some(n) = net.neurons.first_mut() {
                n.squash_type = t;
            }
            let data = build_batched_network_data(&[net], 1, 1)
                .unwrap_or_else(|e| panic!("aggregate squash {t} must be GPU-supported: {e:?}"));
            assert_eq!(
                data.neurons[0].squash_type,
                u32::from(t),
                "aggregate discriminant {t} must serialise unchanged"
            );
        }
    }

    /// Issue #312: HYPOT/HYPOTv2/MEAN (35..=37) are not yet hosted, so they stay
    /// unsupported and force a CPU fallback.
    #[test]
    fn build_batched_network_data_rejects_remaining_aggregate_squashes() {
        for t in 35u8..=37u8 {
            let mut net = synthetic_creature(1, 1, 1);
            if let Some(n) = net.neurons.first_mut() {
                n.squash_type = t;
            }
            let err = build_batched_network_data(&[net], 1, 1)
                .expect_err("remaining aggregate squash must be rejected");
            match err {
                GpuPrepareError::UnsupportedSquash(got) => assert_eq!(got, t),
                other => panic!("expected UnsupportedSquash({t}), got {other:?}"),
            }
        }
    }

    /// Issue #312: constant neurons return a clamped bias and ignore their
    /// synapses on the GPU (the real production creature has 3), so a creature
    /// containing one is now GPU-hostable and its `is_constant` flag is
    /// serialised for the kernel. Its squash discriminant is irrelevant — the
    /// flag short-circuits the squash branch — so even an otherwise-unhosted
    /// aggregate discriminant on a constant neuron must not force a fallback.
    #[test]
    fn build_batched_network_data_accepts_constant_neuron() {
        let mut net = synthetic_creature(1, 1, 1);
        if let Some(n) = net.neurons.first_mut() {
            n.is_constant = true;
            n.squash_type = 37; // MEAN — unhosted, but ignored for constants.
        }
        let data = build_batched_network_data(&[net], 1, 1)
            .expect("a constant neuron is GPU-hostable since Issue #312");
        assert_eq!(
            data.neurons[0].is_constant, 1,
            "the constant flag must be serialised for the kernel"
        );
    }

    /// Issue #312: the IF aggregate needs each synapse's type on the GPU, so the
    /// `SynapseType` discriminant must be widened losslessly into `SynapseGpu`.
    #[test]
    fn build_batched_network_data_populates_synapse_type() {
        let mut net = synthetic_creature(1, 1, 1);
        // Tag the compiled synapses with distinct types and assert they survive
        // the u8 -> u32 widening at the upload boundary.
        for (i, s) in net.synapses.iter_mut().enumerate() {
            s.synapse_type = (i % 4) as u8;
        }
        let expected: Vec<u32> = net
            .synapses
            .iter()
            .map(|s| u32::from(s.synapse_type))
            .collect();
        let data = build_batched_network_data(&[net], 1, 1).expect("supported creature");
        let actual: Vec<u32> = data.synapses.iter().map(|s| s.synapse_type).collect();
        assert_eq!(
            actual, expected,
            "synapse_type must widen u8 -> u32 unchanged"
        );
    }

    /// Issue #182 changed this behaviour: creatures above the 256 private-array
    /// cap are no longer rejected — they route to the `forward_mse_scratch`
    /// kernel. `build_batched_network_data` now accepts a creature just above
    /// the cap; only an absurd count beyond [`MAX_NEURONS_ABSOLUTE`] is refused.
    #[test]
    fn build_batched_network_data_accepts_above_private_cap() {
        let mut net = synthetic_creature(1, 1, 1);
        net.num_neurons = (MAX_NEURONS_PER_CREATURE as usize) + 1;
        let data = build_batched_network_data(&[net], 1, 1)
            .expect("creatures above the 256 cap now route to the scratch kernel");
        assert_eq!(data.creatures.len(), 1);
        assert_eq!(
            data.creatures[0].num_neurons,
            MAX_NEURONS_PER_CREATURE + 1,
            "the serialised neuron count is preserved unchanged"
        );
    }

    #[test]
    fn build_batched_network_data_rejects_absurd_neuron_count() {
        let mut net = synthetic_creature(1, 1, 1);
        net.num_neurons = (MAX_NEURONS_ABSOLUTE as usize) + 1;
        let err = build_batched_network_data(&[net], 1, 1)
            .expect_err("an absurd neuron count is rejected");
        assert!(matches!(err, GpuPrepareError::TooManyNeurons { .. }));
    }

    #[test]
    fn scratch_workgroups_x_is_bounded_by_budget() {
        // A 4139-neuron creature, 50 creatures, 256 MiB budget: G_x must be
        // bounded well below the records-covering grid so scratch stays small.
        let budget = 256 * 1024 * 1024;
        let gx = scratch_workgroups_x_for(1_000_000, 50, 4139, budget);
        assert!(gx >= 1, "G_x is always at least one workgroup");
        let scratch_bytes = 50u64 * gx as u64 * WG_SIZE_X as u64 * 4139 * 4;
        assert!(
            scratch_bytes <= budget,
            "scratch ({scratch_bytes} bytes) must fit the {budget}-byte budget",
        );
    }

    /// Sparse creature: `hidden` non-input neurons, each fed by a couple of
    /// inputs, all feeding one output. Keeps the synapse count low so wide-input
    /// shapes stay cheap to build in a unit test (the dense
    /// [`synthetic_creature`] helper would emit `num_inputs * hidden` synapses).
    fn sparse_creature(num_inputs: usize, hidden: usize) -> CompiledNetwork {
        let mut neurons: Vec<String> = Vec::new();
        for h in 0..hidden {
            neurons.push(format!(
                r#"{{"type":"hidden","uuid":"hidden-{h}","bias":0.05,"squash":"TANH"}}"#
            ));
        }
        neurons
            .push(r#"{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}"#.into());

        let mut synapses: Vec<String> = Vec::new();
        for h in 0..hidden {
            let i = h % num_inputs;
            synapses.push(format!(
                r#"{{"fromUUID":"input-{i}","toUUID":"hidden-{h}","weight":0.1}}"#
            ));
            synapses.push(format!(
                r#"{{"fromUUID":"hidden-{h}","toUUID":"output-0","weight":0.1}}"#
            ));
        }
        if hidden == 0 {
            synapses.push(r#"{"fromUUID":"input-0","toUUID":"output-0","weight":0.1}"#.into());
        }
        let json = format!(
            r#"{{"input":{num_inputs},"output":1,"forwardOnly":true,"semanticVersion":"4.0.0","neurons":[{}],"synapses":[{}]}}"#,
            neurons.join(","),
            synapses.join(","),
        );
        let creature = parse_creature_json(&json).expect("parse creature");
        compile_creature(&creature).expect("compile")
    }

    /// Issue #467 — the Enceladus shape (2461 inputs, 19 hidden, 1 output) is
    /// scratch-routed because inputs count towards `num_neurons`, yet it is
    /// shallow and beats CPU on GPU.
    #[test]
    fn directory_pool_is_shallow_accepts_enceladus_shape() {
        let net = sparse_creature(2461, 19);
        assert!(
            net.num_neurons > MAX_NEURONS_PER_CREATURE as usize,
            "the Enceladus shape must still route to the scratch kernel"
        );
        assert!(directory_pool_is_shallow(std::slice::from_ref(&net)));
        assert_eq!(
            directory_gpu_topology(std::slice::from_ref(&net)),
            DirectoryGpuTopology::ScratchOnly
        );
    }

    /// Issue #467 — the deep production shape (~1666 hidden) stays non-shallow,
    /// preserving the #317 CPU decision.
    #[test]
    fn directory_pool_is_shallow_rejects_deep_production_shape() {
        let net = sparse_creature(2461, 1666);
        assert!(!directory_pool_is_shallow(&[net]));
    }

    /// Issue #467 — the boundary is inclusive at
    /// [`MAX_SHALLOW_NON_INPUT_NEURONS`] non-input neurons (hidden + output).
    #[test]
    fn directory_pool_is_shallow_boundary_is_inclusive() {
        let hidden = MAX_SHALLOW_NON_INPUT_NEURONS as usize - 1;
        let at_cap = sparse_creature(2461, hidden);
        assert_eq!(
            at_cap.num_neurons - at_cap.num_inputs,
            MAX_SHALLOW_NON_INPUT_NEURONS as usize
        );
        assert!(directory_pool_is_shallow(&[at_cap]));

        let over_cap = sparse_creature(2461, hidden + 1);
        assert!(!directory_pool_is_shallow(&[over_cap]));
    }

    /// Issue #467 — one deep creature disqualifies the whole pool, and an empty
    /// pool is never reported as shallow (no evidence ≠ GPU is faster).
    #[test]
    fn directory_pool_is_shallow_requires_every_creature() {
        let shallow = sparse_creature(2461, 19);
        let deep = sparse_creature(2461, 1666);
        assert!(!directory_pool_is_shallow(&[shallow, deep]));
        assert!(!directory_pool_is_shallow(&[]));
    }

    #[test]
    fn effective_directory_gpu_inflight_clamps_scratch_pools() {
        assert_eq!(
            effective_directory_gpu_inflight(DirectoryGpuTopology::AllPrivate, 2),
            2
        );
        assert_eq!(
            effective_directory_gpu_inflight(DirectoryGpuTopology::ScratchOnly, 2),
            1
        );
        assert_eq!(
            effective_directory_gpu_inflight(DirectoryGpuTopology::Mixed, 2),
            1
        );
    }

    #[test]
    fn scratch_workgroups_x_covers_small_record_counts() {
        // With few records the grid never exceeds what is needed to cover them.
        let gx = scratch_workgroups_x_for(100, 4, 300, 512 * 1024 * 1024);
        assert_eq!(gx, 100u32.div_ceil(WG_SIZE_X));
    }

    #[test]
    fn build_batched_network_data_rejects_shape_mismatch() {
        let net1 = synthetic_creature(2, 1, 1);
        let net2 = synthetic_creature(3, 1, 1);
        let err = build_batched_network_data(&[net1, net2], 2, 1)
            .expect_err("input shape mismatch rejected");
        assert!(matches!(err, GpuPrepareError::MismatchedShape));
    }

    // Issue #273 — GPU readback (`map_async`) failure must surface as an `Err`
    // string the `--gpu auto` caller can absorb, not panic the process.

    // Issue #322 — the bind group is reused across dispatches; the pure
    // `bind_group_needs_rebuild` decision must fire only when a bound buffer's
    // generation changes or the scratch binding is resized.

    #[test]
    fn bind_group_rebuilds_when_no_group_cached_yet() {
        let sig = BindGroupSig {
            records_gen: 1,
            partials_gen: 1,
            scratch_gen: 0,
            scratch_bytes: 0,
        };
        // No cached signature (first dispatch) always needs a build.
        assert!(bind_group_needs_rebuild(None, sig));
    }

    #[test]
    fn bind_group_reused_when_signature_unchanged() {
        let sig = BindGroupSig {
            records_gen: 3,
            partials_gen: 2,
            scratch_gen: 0,
            scratch_bytes: 0,
        };
        // Identical signature (steady-state private dispatch) reuses the group.
        assert!(!bind_group_needs_rebuild(Some(sig), sig));
    }

    #[test]
    fn bind_group_rebuilds_when_records_buffer_grows() {
        let cached = BindGroupSig {
            records_gen: 1,
            partials_gen: 1,
            scratch_gen: 0,
            scratch_bytes: 0,
        };
        // The records buffer was reallocated (generation bumped) → rebuild.
        let current = BindGroupSig {
            records_gen: 2,
            ..cached
        };
        assert!(bind_group_needs_rebuild(Some(cached), current));
    }

    #[test]
    fn bind_group_rebuilds_when_scratch_binding_resized() {
        let cached = BindGroupSig {
            records_gen: 1,
            partials_gen: 1,
            scratch_gen: 1,
            scratch_bytes: 4096,
        };
        // Same scratch allocation, but a smaller final chunk binds fewer bytes.
        let current = BindGroupSig {
            scratch_bytes: 2048,
            ..cached
        };
        assert!(bind_group_needs_rebuild(Some(cached), current));
    }

    #[test]
    fn map_readback_result_ok_on_successful_map() {
        // A successful map (`Ok(Ok(()))`) yields `Ok(())`.
        let recv: Result<Result<(), &str>, std::sync::mpsc::RecvError> = Ok(Ok(()));
        assert!(map_readback_result(recv).is_ok());
    }

    #[test]
    fn map_readback_result_err_on_map_failure() {
        // A failed map (`Ok(Err(..))`, e.g. device loss during readback) is
        // turned into a descriptive error instead of panicking.
        let recv: Result<Result<(), &str>, std::sync::mpsc::RecvError> = Ok(Err("device lost"));
        let err = map_readback_result(recv).expect_err("map failure surfaces as Err");
        assert!(
            err.contains("partials map_async failed"),
            "error should name the readback failure, got: {err}",
        );
        assert!(
            err.contains("device lost"),
            "error should carry the underlying cause, got: {err}",
        );
    }

    #[test]
    fn map_readback_result_err_on_dropped_sender() {
        // If the callback sender is dropped before sending, `recv()` returns
        // `RecvError`, which must also become a recoverable `Err`.
        let (sender, receiver) = std::sync::mpsc::channel::<Result<(), &str>>();
        drop(sender);
        let err = map_readback_result(receiver.recv()).expect_err("dropped sender surfaces as Err");
        assert_eq!(err, "partials map_async sender dropped");
    }
}
