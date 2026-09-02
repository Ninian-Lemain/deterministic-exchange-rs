#include "hft_vendor_api.h"

static int token;
static uint32_t sends;
static uint32_t destroys;
static uint32_t last_length;
static uint8_t last_byte;

static int32_t shim_create(void **out_handle) {
    if (out_handle == 0) {
        return 1;
    }
    *out_handle = (void *)&token;
    return 0;
}

static void shim_destroy(void *handle) {
    if (handle == (void *)&token) {
        destroys += 1;
    }
}

static int32_t shim_send(void *handle, const uint8_t *payload, uint32_t length) {
    if (handle != (void *)&token) {
        return 2;
    }
    if (payload == 0 || length == 0u) {
        return 3;
    }
    sends += 1;
    last_length = length;
    last_byte = payload[0];
    return 0;
}

static const hft_vendor_api shim_api = {shim_create, shim_destroy, shim_send};
static const hft_vendor_api null_callback_api = {0, shim_destroy, shim_send};

const hft_vendor_api *hft_test_shim_api(void) {
    return &shim_api;
}

const hft_vendor_api *hft_test_null_callback_api(void) {
    return &null_callback_api;
}

size_t hft_vendor_api_size(void) {
    return sizeof(hft_vendor_api);
}

size_t hft_vendor_api_align(void) {
    return _Alignof(hft_vendor_api);
}

size_t hft_vendor_api_create_offset(void) {
    return offsetof(hft_vendor_api, create);
}

size_t hft_vendor_api_destroy_offset(void) {
    return offsetof(hft_vendor_api, destroy);
}

size_t hft_vendor_api_send_offset(void) {
    return offsetof(hft_vendor_api, send);
}

uint32_t hft_test_shim_sends(void) {
    return sends;
}

uint32_t hft_test_shim_destroys(void) {
    return destroys;
}

uint32_t hft_test_shim_last_length(void) {
    return last_length;
}

uint32_t hft_test_shim_last_byte(void) {
    return (uint32_t)last_byte;
}

void hft_test_shim_reset(void) {
    sends = 0;
    destroys = 0;
    last_length = 0;
    last_byte = 0;
}
