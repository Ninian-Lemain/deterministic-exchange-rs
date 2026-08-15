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
typedef struct hft_vendor_api {
    int32_t (*create)(void **out_handle);
    void (*destroy)(void *handle);
    int32_t (*send)(void *handle, const uint8_t *payload, uint32_t length);
} hft_vendor_api;

_Static_assert(sizeof(hft_vendor_api) == 3 * sizeof(void *), "hft_vendor_api size drift");
_Static_assert(_Alignof(hft_vendor_api) == _Alignof(void *), "hft_vendor_api align drift");

#ifdef __cplusplus
}
#endif

#endif
