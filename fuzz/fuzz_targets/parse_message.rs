#![no_main]

use hft_io::RxFrame;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let frame = RxFrame::from_bytes(data);
    let _ = hft_wire::parse_message(&frame);
});
