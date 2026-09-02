#include "hft_vendor_api.h"

static_assert(sizeof(hft_vendor_api) >= sizeof(hft_vendor_create_fn),
              "hft_vendor_api must be complete");
