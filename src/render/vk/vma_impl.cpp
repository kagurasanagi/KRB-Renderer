// VMA の実装をこの翻訳単位にだけ置く。関数は volk 経由で動的に取得する
#include <volk.h>
#define VMA_STATIC_VULKAN_FUNCTIONS 0
#define VMA_DYNAMIC_VULKAN_FUNCTIONS 1
#define VMA_IMPLEMENTATION
#include <vk_mem_alloc.h>
