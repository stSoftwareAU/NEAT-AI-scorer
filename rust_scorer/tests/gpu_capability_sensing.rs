//! Real-adapter GPU capability sensing (Issue #548).
//!
//! Self-skips on a host with no compatible adapter (CI runs `ubuntu-latest`
//! with none), exactly like the other `gpu_*` integration tests; it executes on
//! the Apple fleet hosts, where it pins the contract the scratch budget depends
//! on: selecting an adapter caches its capability, and the resolved budget
//! never exceeds what that adapter can bind.
//!
//! One test per file (see `gpu_off_no_capability_sensing.rs`): the sensing
//! cache is a process-wide `OnceLock`.

use rust_scorer::gpu::{GpuBackendLabel, select_adapter};
use rust_scorer::host_resources::{default_gpu_scratch_bytes, host, sensed_gpu_capability};

#[test]
fn selecting_an_adapter_senses_the_limits_the_scratch_budget_is_bounded_by() {
    assert_eq!(
        sensed_gpu_capability(),
        None,
        "nothing is sensed until an adapter is selected"
    );

    let Ok(Some(ctx)) = select_adapter() else {
        eprintln!("gpu_capability_sensing: no compatible adapter — skipping");
        return;
    };

    let gpu = sensed_gpu_capability().expect("selecting an adapter senses its capability");
    assert_eq!(
        gpu.backend, ctx.backend,
        "sensed backend matches the context"
    );
    assert_ne!(
        gpu.backend,
        GpuBackendLabel::CpuFallback,
        "a sensed adapter is always a native backend"
    );
    assert!(
        gpu.max_storage_buffer_binding_size > 0,
        "a real adapter reports a positive binding limit"
    );
    assert!(
        gpu.max_compute_workgroups_per_dimension > 0,
        "a real adapter reports a positive per-dimension grid limit"
    );

    let host = host();
    assert_eq!(host.gpu, Some(gpu), "the host snapshot carries the sensing");

    let budget = default_gpu_scratch_bytes(&host);
    assert!(budget > 0, "a sensed host still resolves a positive budget");
    assert!(
        budget <= gpu.max_storage_buffer_binding_size,
        "budget {budget} exceeds the adapter's {} B binding limit",
        gpu.max_storage_buffer_binding_size
    );
    assert!(
        budget.is_power_of_two(),
        "budget {budget} must be a power of two so the scratch allocation \
         cannot round up past the binding limit"
    );
    if gpu.unified_memory
        && let Some(ram) = host.physical_ram_bytes
    {
        assert!(
            budget <= ram / 16,
            "a unified-memory host must keep the scratch budget inside its RAM share"
        );
    }
}
