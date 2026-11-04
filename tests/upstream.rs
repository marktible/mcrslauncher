//! Opt-in contract test against real GitHub releases. No Minecraft account or game assets needed.
use mcrslauncher::{
    engine::Engine,
    model::{Paths, Settings},
};
use std::sync::{Arc, atomic::AtomicBool};

#[test]
#[ignore = "downloads and verifies the official Horizon engine and game build"]
fn official_engine_and_latest_nightly_contract() {
    let temp = tempfile::tempdir().unwrap();
    let engine = Engine::new(
        Paths {
            root: temp.path().to_path_buf(),
        },
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let settings = Settings::default();
    let catalog = engine.catalog(&settings).unwrap();
    println!(
        "Testing {} and {}",
        catalog.engine.tag_name,
        catalog.game_version().unwrap()
    );
    let report = |output| {
        if !matches!(output, mcrslauncher::engine::Output::Progress(_)) {
            println!("{output:?}");
        }
    };
    engine.ensure_engine(&catalog, &report).unwrap();
    engine.verify_local_engine().unwrap();
    let snapshot = engine.snapshot().unwrap();
    assert!(snapshot.accounts.is_empty());
    assert!(!snapshot.owner);
    assert!(
        snapshot.sign_in_configured,
        "Official release must support Microsoft sign-in"
    );
    engine
        .install_game(&catalog, &settings, false, &|o| println!("{o:?}"))
        .unwrap();
    let snapshot = engine.snapshot().unwrap();
    assert!(
        snapshot
            .installed
            .contains(&catalog.game_version().unwrap())
    );
    assert_eq!(
        snapshot.selected_version.as_deref(),
        Some(catalog.game_version().unwrap().as_str())
    );
    // A second install must reuse the same game rather than downloading it again.
    engine
        .install_game(&catalog, &settings, false, &|o| println!("{o:?}"))
        .unwrap();
}
