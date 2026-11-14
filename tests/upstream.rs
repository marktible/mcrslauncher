//! Opt-in contract test against real GitHub releases. No Minecraft account or game assets needed.
use mcrslauncher::{
    engine::Engine,
    model::{Paths, Settings},
};
use std::sync::{Arc, atomic::AtomicBool};

#[test]
#[ignore = "downloads the official game, public Minecraft resources, and Java into an isolated first-run folder"]
fn fresh_setup_without_an_account_or_existing_minecraft() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::env::var_os("MCRS_FIRST_SETUP_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_path_buf());
    let engine = Engine::new(Paths { root }, Arc::new(AtomicBool::new(false))).unwrap();
    let report = |output| match output {
        mcrslauncher::engine::Output::Stage(stage) => println!("{stage}"),
        mcrslauncher::engine::Output::Log(line)
            if line.contains("Java") || line.contains("error:") =>
        {
            println!("{line}")
        }
        _ => {}
    };
    let settings = Settings::default();
    let catalog = engine.catalog(&settings).unwrap();
    engine.ensure_engine(&catalog, &report).unwrap();
    let snapshot = engine.snapshot().unwrap();
    assert!(snapshot.accounts.is_empty());
    assert!(!snapshot.owner);
    engine
        .install_game(&catalog, &settings, false, &report)
        .unwrap();
    engine.prepare(&report).unwrap();
    let snapshot = engine.snapshot().unwrap();
    assert!(snapshot.resources_ready);
    assert!(snapshot.accounts.is_empty());
    assert!(!snapshot.owner);
    let prepared = engine.run(
        &[
            "prepare",
            "--minecraft",
            engine
                .paths
                .engine_home()
                .join("minecraft")
                .to_str()
                .unwrap(),
            "--version",
            snapshot.selected_version.as_deref().unwrap(),
        ],
        &report,
        false,
    );
    assert!(
        prepared.is_ok(),
        "Prepared files must be reusable without signing in: {prepared:?}"
    );
    println!("First-run setup complete with no account and an application-managed resource cache");
}

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
