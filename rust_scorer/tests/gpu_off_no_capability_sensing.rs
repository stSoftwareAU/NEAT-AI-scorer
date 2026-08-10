//! `--gpu off` must not sense — or create — a GPU adapter (Issue #548).
//!
//! GPU capability sensing rides the adapter `gpu::select_adapter` already
//! creates. If it ever grew a probe of its own, `--gpu off` and the GPU-less
//! x86 Linux boxes would pay a `wgpu` initialisation they explicitly declined,
//! and the only fleet-visible symptom would be new startup latency nothing
//! alerts on.
//!
//! This file holds **one** test on purpose: the sensing cache is a process-wide
//! `OnceLock`, so any sibling test that selected an adapter would populate it
//! and mask the leak.

use rust_scorer::gpu::{GpuBackendLabel, GpuMode, resolve_backend};
use rust_scorer::host_resources::{
    HostResources, default_gpu_scratch_bytes, host, sensed_gpu_capability,
};

#[test]
fn gpu_off_senses_no_adapter_and_keeps_the_ram_derived_budget() {
    // Nothing has run a GPU path in this process yet.
    assert_eq!(
        sensed_gpu_capability(),
        None,
        "no adapter can be sensed before one is selected"
    );

    let label = resolve_backend(GpuMode::Off).expect("--gpu off never fails");
    assert_eq!(label, GpuBackendLabel::CpuFallback);

    assert_eq!(
        sensed_gpu_capability(),
        None,
        "--gpu off must not initialise an adapter to sense it"
    );
    let probed = host();
    assert_eq!(probed.gpu, None, "the host snapshot reports no adapter");

    // With nothing sensed the scratch budget is exactly the pre-#548 RAM tier,
    // so a GPU-less host behaves as it always did.
    let no_gpu = HostResources::synthetic_with_performance_cpus(
        probed.cpus,
        probed.performance_cpus,
        probed.physical_ram_bytes,
    );
    let budget = default_gpu_scratch_bytes(&probed);
    assert!(budget > 0, "a no-adapter host still resolves a budget");
    assert_eq!(
        budget,
        default_gpu_scratch_bytes(&no_gpu),
        "the budget cannot depend on an adapter that was never sensed"
    );
}
