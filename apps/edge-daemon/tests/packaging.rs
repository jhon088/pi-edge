//! What the installer has to carry, and what the first launch must not do
//! twice.
//!
//! The Windows installer ships `demo/` as a read-only resource and the first
//! launch imports it. Both halves can break quietly — a photograph that was
//! never referenced, an animation re-encoded by a build step, a second launch
//! stacking another catalog on the first — so both are checked here, on every
//! platform, rather than discovered on a stand.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeSet;

use common::{client, DaemonProcess};
use edge_client::EdgeClient;

fn demo_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("demo")
}

fn seed(database: &std::path::Path) -> serde_json::Value {
    let output = std::process::Command::new(common::cli_binary())
        .arg("--db")
        .arg(database)
        .arg("--format")
        .arg("json")
        .arg("demo-seed")
        .arg("--path")
        .arg(demo_dir())
        .env("PI_EDGE_LOG", "warn")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "demo-seed failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Everything the dataset names is on disk, and nothing on disk is dead
/// weight the installer would carry for no reason.
#[test]
fn the_bundled_demo_is_complete_and_carries_nothing_spare() {
    let demo = demo_dir();
    let menu: serde_json::Value =
        serde_json::from_slice(&std::fs::read(demo.join("menu.json")).unwrap()).unwrap();

    // Every `file:` stem the catalog references.
    fn stems(node: &serde_json::Value, found: &mut BTreeSet<String>) {
        match node {
            serde_json::Value::Object(map) => {
                if let Some(hash) = map.get("content_hash").and_then(|v| v.as_str()) {
                    if let Some(stem) = hash.strip_prefix("file:") {
                        found.insert(stem.to_owned());
                    }
                }
                for value in map.values() {
                    stems(value, found);
                }
            }
            serde_json::Value::Array(items) => {
                for value in items {
                    stems(value, found);
                }
            }
            _ => {}
        }
    }
    let mut referenced = BTreeSet::new();
    stems(&menu, &mut referenced);
    assert!(!referenced.is_empty(), "the dataset references no images");

    let mut on_disk = BTreeSet::new();
    for entry in std::fs::read_dir(demo.join("assets")).unwrap() {
        let path = entry.unwrap().path();
        let extension = path
            .extension()
            .map(|value| value.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp") {
            on_disk.insert(path.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }

    let missing: Vec<&String> = referenced.difference(&on_disk).collect();
    assert!(
        missing.is_empty(),
        "the installer would ship without {missing:?}"
    );
    let spare: Vec<&String> = on_disk.difference(&referenced).collect();
    assert!(spare.is_empty(), "nothing references {spare:?}");
}

/// A build step that "optimises" images would turn the animations into
/// stills, and nobody would notice until the stand.
#[test]
fn the_bundled_animations_are_still_animated() {
    let assets = demo_dir().join("assets");
    let mut animations = 0;
    for entry in std::fs::read_dir(&assets).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().map(|e| e != "gif").unwrap_or(true) {
            continue;
        }
        animations += 1;
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..6], b"GIF89a", "{} is not a GIF", path.display());
        let frames = bytes
            .windows(4)
            .filter(|window| window == b"\x00\x21\xf9\x04")
            .count();
        assert!(
            frames > 1,
            "{} has {frames} frame marker(s); it is a still",
            path.display()
        );
    }
    assert_eq!(animations, 2, "both animated dishes ship");
}

/// The first launch installs the demo. The second must not install it again.
///
/// The guard the installed app uses is the core's own answer to "is a catalog
/// active?", so that is what is asserted here: after one seed the answer is
/// yes, which is what makes the second launch skip. And a seed that *does*
/// run twice must still leave one active menu rather than two.
#[test]
fn seeding_twice_leaves_one_menu_and_one_till() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);

    let first = seed(&database);
    let client = client(&database, "px-kiosk");

    // What the installed app checks before deciding to seed at all.
    let status = client.catalog_status().unwrap();
    assert!(
        status.active.is_some(),
        "a launched kiosk would re-import the catalog on every start"
    );

    let before = client.menu(Some("KIOSK".to_owned())).unwrap();
    let session = client
        .get_edge_state()
        .unwrap()
        .session
        .map(|open| open.session_id);

    // Run it again anyway: idempotent means safe, not merely skipped.
    let second = seed(&database);
    assert_eq!(
        second["assets_imported"].as_i64().unwrap(),
        0,
        "the content-addressed store already had every asset"
    );
    assert!(second["assets_already_present"].as_i64().unwrap() > 0);
    assert_eq!(first["channel"], second["channel"]);

    let after = client.menu(Some("KIOSK".to_owned())).unwrap();
    assert_eq!(
        after.items.len(),
        before.items.len(),
        "the menu doubled; the demo is not idempotent"
    );
    assert_eq!(after.categories.len(), before.categories.len());
    assert_eq!(
        client
            .get_edge_state()
            .unwrap()
            .session
            .map(|open| open.session_id),
        session,
        "a second launch opened a second till"
    );
}

/// The sidecar names the installer stages must be the names Tauri looks for.
///
/// Tauri resolves an `externalBin` entry by appending the target triple and
/// the platform's executable suffix. Nothing checks that the build script
/// agrees with the manifest, so a rename in either would surface as "resource
/// path doesn't exist" halfway through a CI packaging run. This compares the
/// two directly, from the files themselves.
#[test]
fn the_staged_sidecar_names_match_what_tauri_will_look_for() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("apps/tauri-pos/src-tauri/tauri.windows.conf.json")).unwrap(),
    )
    .unwrap();

    let declared: Vec<String> = manifest["bundle"]["externalBin"]
        .as_array()
        .expect("the Windows bundle declares externalBin")
        .iter()
        .map(|entry| entry.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        declared,
        vec![
            "binaries/edge-daemon".to_owned(),
            "binaries/edge-cli".to_owned()
        ],
        "the runtime must travel with the app"
    );

    // What the build script and the workflow actually write.
    let script = std::fs::read_to_string(root.join("scripts/build-windows-installer.ps1")).unwrap();
    let workflow =
        std::fs::read_to_string(root.join(".github/workflows/windows-installer.yml")).unwrap();
    for entry in &declared {
        let name = entry.rsplit('/').next().unwrap();
        // Both producers name the file `<bin>-<triple>.exe`, which is the
        // only spelling Tauri resolves.
        assert!(
            script.contains("$name-$Target.exe"),
            "the build script does not append the target triple"
        );
        assert!(
            workflow.contains("$name-$env:TARGET.exe"),
            "the workflow does not append the target triple"
        );
        assert!(
            script.contains(name) || script.contains("'edge-daemon', 'edge-cli'"),
            "the build script never builds `{name}`"
        );
    }
    assert!(
        script.contains("x86_64-pc-windows-msvc") && workflow.contains("x86_64-pc-windows-msvc"),
        "both producers must target x86_64-pc-windows-msvc"
    );
}

/// The helper that finds `edge-cli` returns something runnable.
///
/// It resolves a sibling of the test's own binary and builds it if absent,
/// which is only correct if the result carries the platform's executable
/// suffix and actually exists. On Windows a missing `.exe` is the difference
/// between a path and a program.
#[test]
fn the_cli_helper_resolves_a_real_executable() {
    let path = common::cli_binary();
    assert!(path.is_file(), "{} is not a file", path.display());
    assert_eq!(
        path.extension().map(|e| e.to_string_lossy().into_owned()),
        if std::env::consts::EXE_SUFFIX.is_empty() {
            None
        } else {
            Some(
                std::env::consts::EXE_SUFFIX
                    .trim_start_matches('.')
                    .to_owned(),
            )
        },
        "the resolved binary must carry this platform's executable suffix"
    );
    // And it runs: the point of resolving it at all.
    let output = std::process::Command::new(&path)
        .arg("--help")
        .output()
        .unwrap_or_else(|error| panic!("{} did not run: {error}", path.display()));
    assert!(output.status.success());
}
