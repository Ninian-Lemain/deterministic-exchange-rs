#![deny(unsafe_op_in_unsafe_fn)]

use core::ffi::c_void;
use core::marker::PhantomData;
use core::ptr::NonNull;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VendorError {
    InvalidApi,
    PayloadTooLarge,
    Status(i32),
}

/// Narrow C ABI supplied by optional C/C++ vendor glue.
///
/// Functions must not unwind or throw across this boundary. A vendor adapter
/// owns the opaque handle and must keep the vtable valid for each session.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct VendorApi {
    pub create: unsafe extern "C" fn(*mut *mut c_void) -> i32,
    pub destroy: unsafe extern "C" fn(*mut c_void),
    pub send: unsafe extern "C" fn(*mut c_void, *const u8, u32) -> i32,
}

const _: () = assert!(core::mem::size_of::<VendorApi>() == 3 * core::mem::size_of::<usize>());
const _: () = assert!(core::mem::align_of::<VendorApi>() == core::mem::align_of::<usize>());

#[inline]
fn checked_payload_len(len: usize) -> Result<u32, VendorError> {
    u32::try_from(len).map_err(|_| VendorError::PayloadTooLarge)
}

/// A session is confined to the thread that opened it:
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<hft_ffi::VendorSession<'static>>();
/// ```
pub struct VendorSession<'api> {
    handle: NonNull<c_void>,
    api: &'api VendorApi,
    not_send_or_sync: PhantomData<Rc<()>>,
}

impl<'api> VendorSession<'api> {
    /// Creates a session through a validated function table.
    ///
    /// # Safety
    ///
    /// Every function pointer must obey its declared C ABI, must not unwind,
    /// and must remain callable for `'api`. On success `create` must initialize
    /// one non-null, uniquely owned handle accepted by `send` and `destroy`.
    ///
    /// # Errors
    ///
    /// Returns [`VendorError::Status`] for a vendor failure or
    /// [`VendorError::InvalidApi`] if success produces a null handle.
    pub unsafe fn open(api: &'api VendorApi) -> Result<Self, VendorError> {
        let mut raw = core::ptr::null_mut();
        // SAFETY: the caller establishes vtable validity and the out-pointer is
        // aligned, writable local storage. The callee may only initialize it.
        let status = unsafe { (api.create)(core::ptr::addr_of_mut!(raw)) };
        if status != 0 {
            return Err(VendorError::Status(status));
        }
        let handle = NonNull::new(raw).ok_or(VendorError::InvalidApi)?;
        Ok(Self {
            handle,
            api,
            not_send_or_sync: PhantomData,
        })
    }

    /// # Errors
    ///
    /// Returns [`VendorError::PayloadTooLarge`] if the slice length does not
    /// fit the C ABI or [`VendorError::Status`] on a vendor failure.
    pub fn send(&mut self, payload: &[u8]) -> Result<(), VendorError> {
        let length = checked_payload_len(payload.len())?;
        // SAFETY: `self` uniquely owns a live handle; `payload` is readable for
        // `length` bytes for the duration of the call, and the vendor contract
        // forbids retention of that pointer and cross-boundary unwinding.
        let status = unsafe { (self.api.send)(self.handle.as_ptr(), payload.as_ptr(), length) };
        if status == 0 {
            Ok(())
        } else {
            Err(VendorError::Status(status))
        }
    }
}

impl Drop for VendorSession<'_> {
    fn drop(&mut self) {
        // SAFETY: `open` established unique ownership. Drop runs once, passes
        // the original aligned opaque pointer, and makes no later calls.
        unsafe { (self.api.destroy)(self.handle.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicUsize, Ordering};

    static SENDS: AtomicUsize = AtomicUsize::new(0);
    static DESTROYS: AtomicUsize = AtomicUsize::new(0);
    static mut TOKEN: u8 = 0;

    unsafe extern "C" fn create(output: *mut *mut c_void) -> i32 {
        if output.is_null() {
            return 1;
        }
        // SAFETY: the test wrapper passes an aligned, writable out-pointer.
        unsafe { output.write(core::ptr::addr_of_mut!(TOKEN).cast()) };
        0
    }

    unsafe extern "C" fn destroy(_handle: *mut c_void) {
        DESTROYS.fetch_add(1, Ordering::Relaxed);
    }

    unsafe extern "C" fn send(_handle: *mut c_void, bytes: *const u8, length: u32) -> i32 {
        if bytes.is_null() || length == 0 {
            return 2;
        }
        SENDS.fetch_add(1, Ordering::Relaxed);
        0
    }

    unsafe extern "C" fn create_failure(_output: *mut *mut c_void) -> i32 {
        7
    }

    unsafe extern "C" fn create_null(output: *mut *mut c_void) -> i32 {
        if output.is_null() {
            return 1;
        }
        // SAFETY: the wrapper passes an aligned, writable out-pointer.
        unsafe { output.write(core::ptr::null_mut()) };
        0
    }

    unsafe extern "C" fn send_failure(_handle: *mut c_void, bytes: *const u8, length: u32) -> i32 {
        if bytes.is_null() || length == 0 {
            return 2;
        }
        3
    }

    static ERROR_DESTROYS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn destroy_after_error(_handle: *mut c_void) {
        ERROR_DESTROYS.fetch_add(1, Ordering::Relaxed);
    }

    #[test]
    fn create_failure_propagates_status() {
        let api = VendorApi {
            create: create_failure,
            destroy,
            send,
        };
        // SAFETY: the test vtable follows the documented ownership and ABI.
        let result = unsafe { VendorSession::open(&api) };
        assert!(matches!(result, Err(VendorError::Status(7))));
    }

    #[test]
    fn null_success_handle_is_invalid_api() {
        let api = VendorApi {
            create: create_null,
            destroy,
            send,
        };
        // SAFETY: the test vtable follows the documented ownership and ABI.
        let result = unsafe { VendorSession::open(&api) };
        assert!(matches!(result, Err(VendorError::InvalidApi)));
    }

    #[test]
    fn send_failure_propagates_and_drop_destroys() {
        let api = VendorApi {
            create,
            destroy: destroy_after_error,
            send: send_failure,
        };
        {
            // SAFETY: the test vtable follows the documented ownership and ABI.
            let mut session = unsafe { VendorSession::open(&api) }.expect("valid test API");
            assert_eq!(session.send(&[1]), Err(VendorError::Status(3)));
        }
        assert_eq!(ERROR_DESTROYS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn oversized_payload_length_is_rejected() {
        let too_large = u32::MAX as usize + 1;
        assert_eq!(
            checked_payload_len(too_large),
            Err(VendorError::PayloadTooLarge)
        );
        assert_eq!(checked_payload_len(1), Ok(1));
    }

    #[test]
    fn wrapper_owns_and_destroys_handle() {
        let api = VendorApi {
            create,
            destroy,
            send,
        };
        {
            // SAFETY: the test vtable follows the documented ownership and ABI.
            let mut session = unsafe { VendorSession::open(&api) }.expect("valid test API");
            session.send(&[1]).expect("vendor send");
        }
        assert_eq!(SENDS.load(Ordering::Relaxed), 1);
        assert_eq!(DESTROYS.load(Ordering::Relaxed), 1);
    }
}
