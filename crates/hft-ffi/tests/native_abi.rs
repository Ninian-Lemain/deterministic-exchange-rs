#![cfg(all(feature = "vendor-sdk", native_shim))]
//! ABI tests against the compiled C shim. Only
//! `rust_session_drives_native_shim` may mutate shim counters because tests
//! run in parallel.

use hft_ffi::{VendorApi, VendorError, VendorSession};

unsafe extern "C" {
    fn hft_test_shim_api() -> *const VendorApi;
    fn hft_vendor_api_size() -> usize;
    fn hft_vendor_api_align() -> usize;
    fn hft_test_shim_sends() -> u32;
    fn hft_test_shim_destroys() -> u32;
    fn hft_test_shim_last_length() -> u32;
    fn hft_test_shim_last_byte() -> u32;
    fn hft_test_shim_reset();
}

#[test]
fn c_layout_matches_rust_layout() {
    // SAFETY: pure getters without side effects or aliasing.
    let (size, align) = unsafe { (hft_vendor_api_size(), hft_vendor_api_align()) };
    assert_eq!(size, core::mem::size_of::<VendorApi>());
    assert_eq!(align, core::mem::align_of::<VendorApi>());
}

#[test]
fn rust_session_drives_native_shim() {
    // SAFETY: plain C functions without aliasing hazards.
    unsafe { hft_test_shim_reset() };
    // SAFETY: points to the static shim vtable honoring the documented ABI.
    let api: &VendorApi = unsafe { &*hft_test_shim_api() };
    {
        // SAFETY: `api` is the compiled C vtable and outlives the session.
        let mut session = unsafe { VendorSession::open(api) }.expect("shim create");
        session.send(&[0xAB, 0xCD]).expect("shim send");
        assert_eq!(session.send(&[]), Err(VendorError::Status(3)));
    }
    // SAFETY: read-only getters.
    let (sends, destroys, last_length, last_byte) = unsafe {
        (
            hft_test_shim_sends(),
            hft_test_shim_destroys(),
            hft_test_shim_last_length(),
            hft_test_shim_last_byte(),
        )
    };
    assert_eq!(sends, 1);
    assert_eq!(destroys, 1);
    assert_eq!(last_length, 2);
    assert_eq!(last_byte, 0xAB);
}
