#pragma once
#include <cstddef>
#include <cstdint>
#include <string>

namespace krbr {

// XXH64（キャッシュのキー用）
uint64_t xxh64(const void* data, size_t len, uint64_t seed = 0);

std::string toHex(uint64_t v);

}  // namespace krbr
