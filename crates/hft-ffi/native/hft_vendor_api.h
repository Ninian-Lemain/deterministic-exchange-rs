#ifndef HFT_VENDOR_API_H
#define HFT_VENDOR_API_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Canonical C ABI for optional vendor or NIC SDK shims. Opaque handles,
   fixed-width fields, explicit ownership, integer error codes, and no
   exceptions or C++ standard-library types across the boundary. */
typedef int32_t (*hft_vendor_create_fn)(void **out_handle);
typedef void (*hft_vendor_destroy_fn)(void *handle);
typedef int32_t (*hft_vendor_send_fn)(void *handle, const uint8_t *payload,
                                      uint32_t length);

typedef struct hft_vendor_api {
    hft_vendor_create_fn create;
    hft_vendor_destroy_fn destroy;
    hft_vendor_send_fn send;
} hft_vendor_api;

#ifdef __cplusplus
static_assert(sizeof(hft_vendor_api) == sizeof(hft_vendor_create_fn) +
                                             sizeof(hft_vendor_destroy_fn) +
                                             sizeof(hft_vendor_send_fn),
              "hft_vendor_api size drift");
static_assert(alignof(hft_vendor_api) == alignof(hft_vendor_create_fn),
              "hft_vendor_api align drift");
#else
_Static_assert(sizeof(hft_vendor_api) == sizeof(hft_vendor_create_fn) +
                                              sizeof(hft_vendor_destroy_fn) +
                                              sizeof(hft_vendor_send_fn),
               "hft_vendor_api size drift");
_Static_assert(_Alignof(hft_vendor_api) == _Alignof(hft_vendor_create_fn),
               "hft_vendor_api align drift");
#endif

#ifdef __cplusplus
}
#endif

#endif
