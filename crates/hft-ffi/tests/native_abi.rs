#![cfg(feature = "vendor-sdk")]
//! ABI tests against the compiled C shim. Only
//! `rust_session_drives_native_shim` may mutate shim counters because tests
//! run in parallel.

use hft_ffi::{VendorApi, VendorError, VendorSession};

unsafe extern "C" {
    safe fn hft_test_shim_api() -> *const VendorApi;
    safe fn hft_test_null_callback_api() -> *const VendorApi;
    safe fn hft_vendor_api_size() -> usize;
    safe fn hft_vendor_api_align() -> usize;
    safe fn hft_vendor_api_create_offset() -> usize;
    safe fn hft_vendor_api_destroy_offset() -> usize;
    safe fn hft_vendor_api_send_offset() -> usize;
    safe fn hft_test_shim_sends() -> u32;
    safe fn hft_test_shim_destroys() -> u32;
    safe fn hft_test_shim_last_length() -> u32;
    safe fn hft_test_shim_last_byte() -> u32;
    safe fn hft_test_shim_reset();
}

fn assert_layout() {
    assert_eq!(hft_vendor_api_size(), core::mem::size_of::<VendorApi>());
    assert_eq!(hft_vendor_api_align(), core::mem::align_of::<VendorApi>());
    assert_eq!(hft_vendor_api_create_offset(), VendorApi::ABI_CREATE_OFFSET);
    assert_eq!(
        hft_vendor_api_destroy_offset(),
        VendorApi::ABI_DESTROY_OFFSET
    );
    assert_eq!(hft_vendor_api_send_offset(), VendorApi::ABI_SEND_OFFSET);
}

#[test]
fn c_layout_matches_rust_layout() {
    assert_layout();
}

#[test]
fn rust_session_drives_native_shim() {
    assert_layout();
    hft_test_shim_reset();
    // SAFETY: the C function returns the static, immutable shim table. Its
    // callbacks follow `VendorApi::new`'s contract.
    let api = unsafe { hft_test_shim_api().as_ref() }.expect("static shim API");
    {
        let mut session = VendorSession::open(api).expect("shim create");
        session.send(&[0xAB, 0xCD]).expect("shim send");
        assert_eq!(session.send(&[]), Err(VendorError::Status(3)));
    }
    let sends = hft_test_shim_sends();
    let destroys = hft_test_shim_destroys();
    let last_length = hft_test_shim_last_length();
    let last_byte = hft_test_shim_last_byte();
    assert_eq!(sends, 1);
    assert_eq!(destroys, 1);
    assert_eq!(last_length, 2);
    assert_eq!(last_byte, 0xAB);
}

#[test]
fn null_callback_is_invalid_api() {
    assert_layout();
    // SAFETY: the C function returns a static, immutable table. Nullable
    // callback fields have a valid Rust representation and `open` checks them.
    let api = unsafe { hft_test_null_callback_api().as_ref() }.expect("static null API");
    assert!(matches!(
        VendorSession::open(api),
        Err(VendorError::InvalidApi)
    ));
}
