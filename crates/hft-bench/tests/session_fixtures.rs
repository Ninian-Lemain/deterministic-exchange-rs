//! Regression coverage for valid admission and sustained window refill.

use hft_bench::session_workloads::{recovery_benchmark, session_benchmark};

#[test]
fn session_cells_check_every_admission_and_refusal() {
    for samples in [1, 2, 3, 4, 2_001] {
        let mut first = Vec::new();
        let mut second = Vec::new();
        session_benchmark(samples, &mut first);
        session_benchmark(samples, &mut second);
        assert_eq!(first.len(), 2);
        for (left, right) in first.iter().zip(&second) {
            assert_eq!(left.samples, samples);
            assert_ne!(left.checksum, 0);
            assert_eq!(left.checksum, right.checksum);
            assert_eq!((left.allocations, left.deallocations), (0, 0));
        }
        assert_ne!(first[0].checksum, first[1].checksum);
    }
}

#[test]
fn recovery_replays_refilled_payloads_after_many_window_wraps() {
    let mut records = Vec::new();
    recovery_benchmark(65, &mut records);
    assert_eq!(records.len(), 1);
    let mut expected = 0_u64;
    for index in 0..65_u64 {
        // Sample zero retains 33..=96, then the window advances by 32.
        for sequence in 33 + index * 32..=96 + index * 32 {
            expected = expected.rotate_left(7) ^ sequence;
            let mut payload = [0_u8; 46];
            payload[..8].copy_from_slice(&sequence.to_be_bytes());
            for byte in payload {
                expected = expected.rotate_left(3) ^ u64::from(byte);
            }
        }
    }
    assert_eq!(records[0].samples, 65);
    assert_eq!(records[0].checksum, expected);
    assert_eq!((records[0].allocations, records[0].deallocations), (0, 0));
}

#[test]
fn empty_sample_sets_emit_no_session_cells() {
    let mut records = Vec::new();
    session_benchmark(0, &mut records);
    recovery_benchmark(0, &mut records);
    assert!(records.is_empty());
}
