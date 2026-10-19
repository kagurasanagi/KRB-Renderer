// krbr のテスト（doctest）。ウィンドウは開かない。GPU を使うテストはオフスクリーンで描く
#define DOCTEST_CONFIG_IMPLEMENT
#include <doctest/doctest.h>

#include "core/log.hpp"
#include "core/platform.hpp"

int main(int argc, char** argv) {
    krbr::platform::setLowPriority();
    krbr::log::setLevel(krbr::log::Level::Warn);
    doctest::Context ctx;
    ctx.applyCommandLine(argc, argv);
    return ctx.run();
}
