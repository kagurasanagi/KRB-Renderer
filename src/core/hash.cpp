#include "core/hash.hpp"

#include <cstring>

namespace krbr {
namespace {
constexpr uint64_t P1 = 11400714785074694791ULL;
constexpr uint64_t P2 = 14029467366897019727ULL;
constexpr uint64_t P3 = 1609587929392839161ULL;
constexpr uint64_t P4 = 9650029242287828579ULL;
constexpr uint64_t P5 = 2870177450012600261ULL;

inline uint64_t rotl(uint64_t x, int r) { return (x << r) | (x >> (64 - r)); }
inline uint64_t read64(const unsigned char* p) {
    uint64_t v;
    std::memcpy(&v, p, 8);
    return v;
}
inline uint32_t read32(const unsigned char* p) {
    uint32_t v;
    std::memcpy(&v, p, 4);
    return v;
}
inline uint64_t round(uint64_t acc, uint64_t input) {
    acc += input * P2;
    acc = rotl(acc, 31);
    return acc * P1;
}
inline uint64_t mergeRound(uint64_t acc, uint64_t val) {
    acc ^= round(0, val);
    return acc * P1 + P4;
}
}  // namespace

uint64_t xxh64(const void* data, size_t len, uint64_t seed) {
    const auto* p = static_cast<const unsigned char*>(data);
    const unsigned char* end = p + len;
    uint64_t h;
    if (len >= 32) {
        const unsigned char* limit = end - 32;
        uint64_t v1 = seed + P1 + P2, v2 = seed + P2, v3 = seed, v4 = seed - P1;
        do {
            v1 = round(v1, read64(p));
            v2 = round(v2, read64(p + 8));
            v3 = round(v3, read64(p + 16));
            v4 = round(v4, read64(p + 24));
            p += 32;
        } while (p <= limit);
        h = rotl(v1, 1) + rotl(v2, 7) + rotl(v3, 12) + rotl(v4, 18);
        h = mergeRound(h, v1);
        h = mergeRound(h, v2);
        h = mergeRound(h, v3);
        h = mergeRound(h, v4);
    } else {
        h = seed + P5;
    }
    h += static_cast<uint64_t>(len);
    while (p + 8 <= end) {
        h ^= round(0, read64(p));
        h = rotl(h, 27) * P1 + P4;
        p += 8;
    }
    if (p + 4 <= end) {
        h ^= static_cast<uint64_t>(read32(p)) * P1;
        h = rotl(h, 23) * P2 + P3;
        p += 4;
    }
    while (p < end) {
        h ^= static_cast<uint64_t>(*p) * P5;
        h = rotl(h, 11) * P1;
        ++p;
    }
    h ^= h >> 33;
    h *= P2;
    h ^= h >> 29;
    h *= P3;
    h ^= h >> 32;
    return h;
}

std::string toHex(uint64_t v) {
    static const char* digits = "0123456789abcdef";
    std::string s(16, '0');
    for (int i = 15; i >= 0; --i) {
        s[i] = digits[v & 0xF];
        v >>= 4;
    }
    return s;
}

}  // namespace krbr
