set(VCPKG_TARGET_ARCHITECTURE x64)
set(VCPKG_CRT_LINKAGE dynamic)
set(VCPKG_LIBRARY_LINKAGE dynamic)
# Rust debug builds also use the release MSVC runtime. Avoid building a second
# native debug dependency tree that no binary or test links against.
set(VCPKG_BUILD_TYPE release)
