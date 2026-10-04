fn main() {
    println!("cargo:rerun-if-changed=native/backend.cpp");
    println!("cargo:rerun-if-env-changed=LIBTORRENT_PREFIX");
    let mut build = cc::Build::new();
    build.cpp(true).std("c++17").file("native/backend.cpp");
    if std::env::var_os("CARGO_FEATURE_INTEGRATION_TESTS").is_some() {
        build.define("TS_INTEGRATION_TESTS", None);
    }
    if let Ok(prefix) = std::env::var("LIBTORRENT_PREFIX") {
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
