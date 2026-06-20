//! End-to-end integration tests for the public [`AssetStore`] surface.
//!
//! These exercise [`LocalCasAssetStore`] strictly through the frozen public API
//! (as a downstream crate would), proving the definition-of-done behaviors:
//! opaque ensure/materialize/gc round-trips (byte-identical, read-only),
//! platform-keyed deps deny by default, and gc keeps live while reclaiming dead.

use std::fs;
use std::path::Path;

use spork_asset::{AssetError, AssetKey, AssetStore, LocalCasAssetStore, Reclaimed};
use spork_hash::Hash;
use tempfile::TempDir;

/// Deterministic pseudo-random bytes (no rng nondeterminism), so a "large"
/// artifact exercises the CAS chunker without flakiness.
fn pseudo_random(n: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        out.push((state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as u8);
    }
    out
}

fn collect(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            collect(base, &path, out);
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .to_string();
            out.push((rel, fs::read(&path).unwrap()));
        }
    }
}

#[test]
fn opaque_large_file_round_trips_byte_identical_and_read_only() {
    let tmp = TempDir::new().unwrap();
    let store = LocalCasAssetStore::open(tmp.path().join("cache")).unwrap();

    // A multi-megabyte opaque artifact (drives FastCDC in the backing CAS).
    let src = tmp.path().join("model.weights");
    let data = pseudo_random(3 * 1024 * 1024, 1234);
    fs::write(&src, &data).unwrap();

    let content_hash = store.content_hash_for_source(&src).unwrap();
    let key = AssetKey::opaque(content_hash);
    let asset = store.ensure(&key, Some(&src)).unwrap();
    assert_eq!(asset.stored, content_hash);

    let dest = tmp.path().join("sandbox/model.weights");
    store.materialize(&key, &dest).unwrap();
    assert_eq!(
        fs::read(&dest).unwrap(),
        data,
        "materialized bytes must match"
    );
    assert!(
        fs::metadata(&dest).unwrap().permissions().readonly(),
        "materialized opaque asset must be read-only"
    );

    // Materializing again (e.g. into a second sandbox) is fine and consistent.
    let dest2 = tmp.path().join("sandbox2/model.weights");
    store.materialize(&key, &dest2).unwrap();
    assert_eq!(fs::read(&dest2).unwrap(), data);
}

#[test]
fn opaque_directory_round_trips() {
    let tmp = TempDir::new().unwrap();
    let store = LocalCasAssetStore::open(tmp.path().join("cache")).unwrap();

    let src = tmp.path().join("vendored");
    fs::create_dir_all(src.join("pkg/sub")).unwrap();
    fs::write(src.join("LICENSE"), b"MIT").unwrap();
    fs::write(src.join("pkg/index.js"), b"module.exports = {}").unwrap();
    fs::write(
        src.join("pkg/sub/data.bin"),
        pseudo_random(2 * 1024 * 1024, 77),
    )
    .unwrap();

    let content_hash = store.content_hash_for_source(&src).unwrap();
    let key = AssetKey::opaque(content_hash);
    store.ensure(&key, Some(&src)).unwrap();

    let dest = tmp.path().join("restored");
    store.materialize(&key, &dest).unwrap();

    let mut original = Vec::new();
    let mut restored = Vec::new();
    collect(&src, &src, &mut original);
    collect(&dest, &dest, &mut restored);
    original.sort();
    restored.sort();
    assert_eq!(
        original, restored,
        "directory asset must round-trip byte-for-byte"
    );
}

#[test]
fn deps_key_with_platform_denies_by_default() {
    let tmp = TempDir::new().unwrap();
    let store = LocalCasAssetStore::open(tmp.path().join("cache")).unwrap();

    let key = AssetKey::deps("npm", Hash::from_bytes([0x11; 32]), "aarch64-apple-darwin");
    let err = store.ensure(&key, None).unwrap_err();
    match err {
        AssetError::EcosystemNotRegistered(eco) => assert_eq!(eco, "npm"),
        other => panic!("expected EcosystemNotRegistered, got {other:?}"),
    }
}

#[test]
fn gc_keeps_live_and_reclaims_dead() {
    let tmp = TempDir::new().unwrap();
    let store = LocalCasAssetStore::open(tmp.path().join("cache")).unwrap();

    let a = tmp.path().join("a.bin");
    let b = tmp.path().join("b.bin");
    fs::write(&a, pseudo_random(1024 * 1024, 1)).unwrap();
    fs::write(&b, pseudo_random(1024 * 1024, 2)).unwrap();
    let ka = AssetKey::opaque(store.content_hash_for_source(&a).unwrap());
    let kb = AssetKey::opaque(store.content_hash_for_source(&b).unwrap());
    store.ensure(&ka, Some(&a)).unwrap();
    store.ensure(&kb, Some(&b)).unwrap();

    // Keep A only.
    let reclaimed = store.gc(std::slice::from_ref(&ka)).unwrap();
    assert!(reclaimed.objects > 0);
    assert!(reclaimed.bytes > 0);

    // A survives and re-materializes; B is gone.
    let out_a = tmp.path().join("out_a.bin");
    store.materialize(&ka, &out_a).unwrap();
    assert_eq!(fs::read(&out_a).unwrap(), fs::read(&a).unwrap());
    assert!(matches!(
        store.materialize(&kb, &tmp.path().join("out_b.bin")),
        Err(AssetError::SourceRequired(_))
    ));

    // A second GC keeping A reclaims nothing further (idempotent steady state).
    assert_eq!(store.gc(&[ka]).unwrap(), Reclaimed::default());
}
