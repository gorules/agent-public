use std::path::{Path, PathBuf};

/// Precompiles the tsgo WASI module so startup deserializes an artifact
/// instead of spending ~13 CPU-seconds compiling one.
///
/// The target triple is always pinned, even when it equals the host. Left to
/// its own devices cranelift emits code for the CPU it detects on the build
/// machine, so an image built on a newer core produces a `.cwasm` that older
/// hosts of the same triple refuse to load.
///
/// `TSGO_CWASM_CACHE` points at a directory the precompiled artifact is kept
/// in, so CI can restore it instead of rebuilding it every run. A cached
/// artifact is only reused if it still loads and was built from the same tsgo
/// revision and target — a stale one is rebuilt rather than shipped.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=TSGO_CWASM_CACHE");
    println!("cargo:rerun-if-env-changed=TSGO_WASM_FILE");

    let target = std::env::var("TARGET").expect("cargo sets TARGET");
    let out =
        PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("tsgo.cwasm.zst");

    let cache = std::env::var("TSGO_CWASM_CACHE").ok().map(PathBuf::from);
    let stamp = format!("rev={} target={target}", tsgo_wasm::TSGO_REV.trim());

    if let Some(cache) = &cache
        && let Some(cached) = usable(cache, &stamp)
    {
        std::fs::copy(cached, &out).expect("copy cached cwasm");
        return;
    }

    let cwasm = tsgo_wasm::TypeScriptConfig::default()
        .precompile(Some(&target))
        .expect("precompile tsgo");
    let compressed = zstd::encode_all(cwasm.as_slice(), 3).expect("compress cwasm");
    std::fs::write(&out, &compressed).expect("write cwasm");

    if let Some(cache) = &cache {
        std::fs::create_dir_all(cache).expect("create cwasm cache dir");
        std::fs::write(cache.join("tsgo.cwasm.zst"), &compressed).expect("cache cwasm");
        std::fs::write(cache.join("tsgo.cwasm.stamp"), &stamp).expect("cache cwasm stamp");
    }
}

/// A cached artifact is trusted only if the stamp matches *and* it still
/// deserializes. The stamp catches a changed module or target; the load catches
/// everything else that invalidates a cwasm — a wasmtime bump above all — which
/// is what stops a stale cache from silently shipping a module that fails at
/// runtime instead of at build time.
fn usable(cache: &Path, stamp: &str) -> Option<PathBuf> {
    let cwasm = cache.join("tsgo.cwasm.zst");
    if std::fs::read_to_string(cache.join("tsgo.cwasm.stamp"))
        .ok()?
        .trim()
        != stamp
    {
        return None;
    }

    let bytes = std::fs::read(&cwasm).ok()?;
    match unsafe { tsgo_wasm::TypeScript::from_cwasm(&bytes) } {
        Ok(_) => Some(cwasm),
        Err(error) => {
            println!("cargo:warning=cached tsgo cwasm rejected, rebuilding: {error}");
            None
        }
    }
}
