//! GPU/cost/racing run plan for the CLI — Issue #648.
//!
//! [`super::run`] kept absorbing a new guard or `match` arm for every GPU,
//! cost and racing issue. The decisions now live here as small functions that
//! unit tests reach directly, so `run` only sequences host-report / stdin /
//! directory / single-file dispatch.

use std::path::Path;
use std::sync::Arc;

use crate::cost::CostKind;
use crate::gpu::{self, GpuBackendLabel, GpuContext, GpuMode, ScoringPath};
use crate::multi_score;

/// An adapter chosen for a run: the `gpuBackend` label and the shared context.
pub(super) type GpuSelection = (GpuBackendLabel, Arc<GpuContext>);

/// Refuse flag combinations that cannot be honoured, before any `wgpu` call.
///
/// # Errors
///
/// A message naming the conflicting flags.
pub(super) fn validate_flag_combinations(
    mode: GpuMode,
    cost: CostKind,
    race_stdio: bool,
    creature_stdin: bool,
) -> Result<(), String> {
    // NEAT-AI#3928: `--race-stdio` drives the Issue #308 early-exit hook, which
    // exists only on the CPU directory path. Refuse the combinations that could
    // not honour it rather than silently full-scoring a caller who asked to
    // race — a full-corpus sweep is exactly what racing is meant to avoid, and
    // it would look like a working race that never saved anything.
    if race_stdio {
        if creature_stdin {
            return Err(
                "--race-stdio scores a creatures directory; it cannot be combined with --creature-stdin"
                    .to_string(),
            );
        }
        if matches!(mode, GpuMode::On) {
            return Err(
                "--race-stdio has no GPU kernel: the early-exit hook is CPU directory mode only (drop --gpu on, or drop --race-stdio)"
                    .to_string(),
            );
        }
    }

    // Issue #121/#339/#316: `--gpu on` with a cost the kernels cannot serve is a
    // hard error. The batched/scratch kernels host MSE and RMSE (squared-error
    // sum, RMSE via a host-side `sqrt`) and MAE (absolute-error sum); every
    // other cost would be a wrong scoring result if silently downgraded. Checked
    // before adapter selection so the error always mentions the unsupported
    // cost, even on machines without a GPU. `--gpu auto` (the default) falls
    // back to CPU instead; `--gpu off` never touches the GPU and is fine.
    if matches!(mode, GpuMode::On) && !cost.gpu_supported() {
        return Err(format!(
            "GPU kernel not implemented for cost {}: the batched/scratch kernels host MSE, RMSE \
             and MAE only (use --gpu auto to silently fall back to CPU, or --gpu off to skip GPU detection)",
            cost.as_str()
        ));
    }
    Ok(())
}

/// Select the adapter up-front — `--gpu on` only.
///
/// Issue #180: under `--gpu auto` adapter selection is *deferred* to the
/// directory path, where a CPU-only pre-flight first checks that the GPU kernel
/// can host the creature set. Creating a `wgpu`/Metal device only to abandon it
/// risked an abnormal teardown that truncated stdout (`exit 158` /
/// `INVALID_JSON`). `on` still resolves here — the user demanded a GPU, so a
/// missing adapter must hard-error.
///
/// # Errors
///
/// Under `--gpu on`: no adapter, a selection error, or a driver abort.
pub(super) fn select_up_front(mode: GpuMode) -> Result<Option<GpuSelection>, String> {
    if !matches!(mode, GpuMode::On) {
        return Ok(None);
    }
    // Issue #583: adapter selection is a `wgpu` call too, and `wgpu` reports a
    // dead device fatally — guard it so even a driver that dies during
    // selection exits with a diagnostic instead of a panic.
    match gpu::device_loss::catch_gpu_panic(gpu::select_adapter) {
        Ok(Ok(Some(ctx))) => Ok(Some((ctx.backend, Arc::new(ctx)))),
        Ok(Ok(None)) => Err(
            "No compatible GPU adapter found and --gpu on was requested (use --gpu auto to fall back to CPU, or --gpu off to skip GPU detection entirely)".to_string(),
        ),
        Ok(Err(e)) => Err(e.to_string()),
        Err(failure) => Err(failure.to_string()),
    }
}

/// Whether a creatures directory wants the GPU kernel, plus the one-line
/// `[gpu] auto fallback …` stderr notes that explain a declined GPU.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct DirectoryGpuPlan {
    pub(super) want_gpu: bool,
    pub(super) notes: Vec<String>,
}

/// Route a creatures directory: `Off` ⇒ CPU, `On` ⇒ GPU, `Auto` ⇒ cost- and
/// topology-aware (Issues #83, #205, #317, #467). Never creates a GPU device.
pub(super) fn plan_directory(
    mode: GpuMode,
    cost: CostKind,
    creature_dir: &Path,
) -> DirectoryGpuPlan {
    let mut notes = Vec::new();
    // Issue #205: name a non-GPU cost as the reason for an `auto` CPU fallback.
    notes.extend(gpu::auto_cost_fallback_note(
        mode,
        ScoringPath::CreatureDirectory,
        cost,
    ));
    // Issue #467: the topology probe loads and compiles every creature, so run
    // it once and share it between the note and the routing decision. Only
    // `auto` with a GPU-hosted cost consults it.
    let probe = if matches!(mode, GpuMode::Auto) && cost.gpu_supported() {
        multi_score::gpu_directory_probe_for_dir(creature_dir)
    } else {
        None
    };
    notes.extend(gpu::auto_topology_fallback_note(
        mode,
        ScoringPath::CreatureDirectory,
        cost,
        probe,
    ));
    let want_gpu = match mode {
        GpuMode::Off => false,
        GpuMode::On => true,
        GpuMode::Auto => gpu::auto_should_use_gpu_directory(probe, cost),
    };
    DirectoryGpuPlan { want_gpu, notes }
}

/// The adapter a GPU-wanting directory run uses, or `None` to run on CPU.
///
/// `on` reuses the [`select_up_front`] selection. `auto` (Issue #180) runs the
/// CPU-only pre-flight first: a set the kernel cannot host routes straight to
/// CPU *without* creating a device, so there is no context to abort during
/// teardown.
pub(super) fn resolve_directory_gpu(
    mode: GpuMode,
    up_front: Option<GpuSelection>,
    creature_dir: &Path,
) -> Option<GpuSelection> {
    match mode {
        GpuMode::On => up_front,
        GpuMode::Auto => match multi_score::gpu_directory_compatible(creature_dir) {
            // GPU-hostable — create the adapter now. Guarded since Issue #583:
            // a driver that dies during selection must not take the process
            // with it. No adapter, a selection error or a driver abort all fall
            // through to CPU — `auto` must never abort scoring.
            Ok(()) => match gpu::device_loss::catch_gpu_panic(gpu::select_adapter) {
                Ok(Ok(Some(ctx))) => Some((ctx.backend, Arc::new(ctx))),
                _ => None,
            },
            // The set exceeds the shader cap (or uses an unsupported squash).
            // Log the fallback and run on CPU — no device made.
            Err(reason) => {
                eprintln!(
                    "[gpu] auto fallback to CPU directory mode: GPU runner cannot host this creature set ({reason}); rerun with --gpu off"
                );
                None
            }
        },
        // `plan_directory` never wants GPU under Off.
        GpuMode::Off => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_json::{creature_envelope, neuron_json, synapse_json};

    const CPU_ONLY_COSTS: [CostKind; 5] = [
        CostKind::Mape,
        CostKind::Msle,
        CostKind::Hinge,
        CostKind::CrossEntropy,
        CostKind::CategoricalError,
    ];

    /// `hidden` IDENTITY neurons fanning `inputs` into one output.
    fn creature_json(inputs: usize, hidden: usize) -> String {
        let mut neurons = Vec::new();
        let mut synapses = Vec::new();
        for h in 0..hidden {
            let uuid = format!("h{h}");
            neurons.push(neuron_json("hidden", &uuid, 0.0, "IDENTITY"));
            synapses.push(synapse_json(&format!("input-{}", h % inputs), &uuid, 1.0));
            synapses.push(synapse_json(&uuid, "o0", 1.0));
        }
        neurons.push(neuron_json("output", "o0", 0.0, "IDENTITY"));
        creature_envelope(inputs, 1, &neurons, &synapses)
    }

    fn dir_with(tmp: &tempfile::TempDir, json: &str) -> std::path::PathBuf {
        let dir = tmp.path().join("creatures");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("c.json"), json).expect("write");
        dir
    }

    #[test]
    fn validate_accepts_every_honourable_combination() {
        for mode in [GpuMode::Off, GpuMode::Auto, GpuMode::On] {
            for cost in [CostKind::Mse, CostKind::Rmse, CostKind::Mae] {
                validate_flag_combinations(mode, cost, false, false).expect("GPU cost");
                validate_flag_combinations(mode, cost, false, true).expect("stdin");
            }
        }
        for cost in CPU_ONLY_COSTS {
            validate_flag_combinations(GpuMode::Auto, cost, true, false).expect("auto race");
            validate_flag_combinations(GpuMode::Off, cost, true, false).expect("off race");
        }
    }

    #[test]
    fn validate_refuses_race_stdio_with_creature_stdin() {
        let err = validate_flag_combinations(GpuMode::Auto, CostKind::Mse, true, true)
            .expect_err("race + stdin");
        assert!(err.contains("--creature-stdin"), "{err}");
        // Checked first: the stdin conflict wins over `--gpu on`.
        let err = validate_flag_combinations(GpuMode::On, CostKind::Mse, true, true)
            .expect_err("race + stdin + on");
        assert!(err.contains("--creature-stdin"), "{err}");
    }

    #[test]
    fn validate_refuses_race_stdio_with_gpu_on() {
        let err = validate_flag_combinations(GpuMode::On, CostKind::Mse, true, false)
            .expect_err("race + on");
        assert!(err.contains("no GPU kernel"), "{err}");
    }

    #[test]
    fn validate_refuses_gpu_on_with_cpu_only_cost() {
        for cost in CPU_ONLY_COSTS {
            let err = validate_flag_combinations(GpuMode::On, cost, false, false)
                .expect_err("on + CPU-only cost");
            assert!(err.contains(cost.as_str()), "must name the cost: {err}");
        }
    }

    /// Issue #180: only `on` selects an adapter up-front, so `off` and `auto`
    /// resolve to nothing without touching `wgpu`.
    #[test]
    fn select_up_front_defers_for_off_and_auto() {
        assert!(select_up_front(GpuMode::Off).expect("off").is_none());
        assert!(select_up_front(GpuMode::Auto).expect("auto").is_none());
    }

    #[test]
    fn plan_off_and_on_ignore_topology() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dir_with(&tmp, &creature_json(300, 300));
        let off = plan_directory(GpuMode::Off, CostKind::Mape, &dir);
        assert_eq!(
            off,
            DirectoryGpuPlan {
                want_gpu: false,
                notes: vec![]
            }
        );
        let on = plan_directory(GpuMode::On, CostKind::Mse, &dir);
        assert_eq!(
            on,
            DirectoryGpuPlan {
                want_gpu: true,
                notes: vec![]
            }
        );
    }

    #[test]
    fn plan_auto_cpu_only_cost_falls_back_with_cost_note() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dir_with(&tmp, &creature_json(2, 1));
        let plan = plan_directory(GpuMode::Auto, CostKind::Hinge, &dir);
        assert!(!plan.want_gpu);
        assert_eq!(plan.notes.len(), 1, "{:?}", plan.notes);
        assert!(plan.notes[0].contains("cost HINGE"), "{:?}", plan.notes);
    }

    #[test]
    fn plan_auto_all_private_pool_wants_gpu_silently() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dir_with(&tmp, &creature_json(2, 1));
        let plan = plan_directory(GpuMode::Auto, CostKind::Mse, &dir);
        assert_eq!(
            plan,
            DirectoryGpuPlan {
                want_gpu: true,
                notes: vec![]
            }
        );
    }

    /// Issue #317: 300 inputs + 300 hidden exceeds both the 256-neuron private
    /// cap and the #467 shallow cap, so `auto` declines GPU and says why.
    #[test]
    fn plan_auto_deep_scratch_pool_falls_back_with_topology_note() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dir_with(&tmp, &creature_json(300, 300));
        let plan = plan_directory(GpuMode::Auto, CostKind::Mse, &dir);
        assert!(!plan.want_gpu);
        assert_eq!(plan.notes.len(), 1, "{:?}", plan.notes);
        assert!(
            plan.notes[0].contains("deep scratch-kernel"),
            "{:?}",
            plan.notes
        );
    }

    /// Issue #180: a GPU-wanting `off` run (unreachable in practice) and an
    /// `on` run with no up-front selection both resolve to CPU.
    #[test]
    fn resolve_directory_gpu_without_selection_is_cpu() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = dir_with(&tmp, &creature_json(2, 1));
        assert!(resolve_directory_gpu(GpuMode::Off, None, &dir).is_none());
        assert!(resolve_directory_gpu(GpuMode::On, None, &dir).is_none());
    }
}
