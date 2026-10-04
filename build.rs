fn main() {
    println!("cargo:rerun-if-changed=native/backend.cpp");
    println!("cargo:rerun-if-env-changed=LIBTORRENT_PREFIX");
    let mut build = cc::Build::new();
    build.cpp(true).std("c++17").file("native/backend.cpp");
    if std::env::var_os("CARGO_FEATURE_INTEGRATION_TESTS").is_some() {
        build.define("TS_INTEGRATION_TESTS", None);
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let lib = vcpkg::Config::new().find_package("libtorrent").expect(
            "Install libtorrent[core,deprfun] 2.x with vcpkg and set VCPKG_ROOT and VCPKGRS_TRIPLET (see README)",
        );
        for path in lib.include_paths {
            build.include(path);
        }
        for name in [
            "TORRENT_USE_OPENSSL",
            "TORRENT_USE_LIBCRYPTO",
            "TORRENT_SSL_PEERS",
            "BOOST_ALL_NO_LIB",
        ] {
            build.define(name, None);
        }
        // The deprfun vcpkg feature preserves the 2.0 file-storage API in 2.1.
        build.define("TORRENT_ABI_VERSION", "2");
        build.define("_WIN32_WINNT", "0x0A00");
        if !lib.is_static {
            build.define("TORRENT_LINKING_SHARED", None);
        }
        build.flag("/EHsc").flag("/utf-8");
        for lib in ["ws2_32", "crypt32", "bcrypt", "iphlpapi"] {
            println!("cargo:rustc-link-lib={lib}");
        }
    } else if let Ok(prefix) = std::env::var("LIBTORRENT_PREFIX") {
        build.include(format!("{prefix}/include"));
        for name in [
            "TORRENT_LINKING_SHARED",
            "TORRENT_USE_OPENSSL",
            "TORRENT_USE_LIBCRYPTO",
            "TORRENT_SSL_PEERS",
        ] {
            build.define(name, None);
        }
        println!("cargo:rustc-link-search=native={prefix}/lib/x86_64-linux-gnu");
        println!("cargo:rustc-link-search=native={prefix}/lib");
        println!("cargo:rustc-link-lib=torrent-rasterbar");
        println!("cargo:rustc-link-lib=ssl");
        println!("cargo:rustc-link-lib=crypto");
    } else {
        let lib = pkg_config::Config::new().range_version("2.0".."3.0").probe("libtorrent-rasterbar")
            .expect("Install libtorrent-rasterbar-dev, libboost-dev, libssl-dev and pkg-config (libtorrent 2.x required)");
        for path in lib.include_paths {
            build.include(path);
        }
        for (name, value) in lib.defines {
            build.define(&name, value.as_deref());
        }
    }
    build.compile("torrent_stream_backend");
}
