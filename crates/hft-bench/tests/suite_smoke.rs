//! Reduced-suite smoke check in its own process so the global counting
//! allocator is not perturbed by sibling tests.

use hft_bench::{SuiteConfig, run_suite};

#[test]
fn reduced_suite_emits_records_with_zero_allocations() {
    if hft_spsc::IS_LOOM_BUILD {
        eprintln!("skipping: loom build cannot execute the timed suite");
        return;
    }
    let lines = run_suite(SuiteConfig::reduced());
    assert!(lines.len() > 40, "suite emitted {}", lines.len());
    for line in &lines {
        assert!(line.contains("\"allocations\":0"), "{line}");
        assert!(line.contains("\"deallocations\":0"), "{line}");
    }
}
