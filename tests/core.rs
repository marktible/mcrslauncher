#[cfg(unix)]
use mcrslauncher::engine::Engine;
use mcrslauncher::{
    engine::parse_login,
    github::{self, Asset, Release},
    model::{Channel, Paths, Settings},
};
use std::fs;
#[cfg(unix)]
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

fn asset(name: &str) -> Asset {
    Asset {
        name: name.into(),
        browser_download_url: format!(
            "https://github.com/horizon-rust/horizon/releases/download/test/{name}"
        ),
        url: None,
        size: 4,
        digest: Some(format!("sha256:{}", github::digest_hex(b"test"))),
        updated_at: "2026-11-04T01:00:00Z".into(),
    }
}

fn release(tag: &str, date: &str, prerelease: bool, names: &[&str]) -> Release {
    Release {
        tag_name: tag.into(),
        name: tag.into(),
        body: String::new(),
        published_at: Some(date.into()),
        prerelease,
        draft: false,
        assets: names.iter().map(|n| asset(n)).collect(),
    }
}

fn releases() -> Vec<Release> {
    let mut nightly = release(
        "nightly",
        "2026-11-04",
        true,
        &["horizon-linux-x86_64.tar.gz"],
    );
    nightly.name = "Nightly 2026-11-04 · 67dad0d".into();
    vec![
        release(
            "launcher@0.2.38",
            "2026-11-04",
            false,
            &["horizon-launcher-linux-x86_64.tar.gz"],
        ),
        release(
            "launcher@0.2.37",
            "2026-11-03",
            false,
            &["horizon-launcher-linux-x86_64.tar.gz"],
        ),
        release(
            "v0.2.99",
            "2026-10-27",
            false,
            &["horizon-linux-x86_64.tar.gz"],
        ),
        nightly,
    ]
}

#[test]
fn latest_game_does_not_confuse_launcher_releases_with_game_releases() {
    let all = releases();
    let catalog = github::select_catalog(&all, Channel::Nightly, "linux", "x86_64").unwrap();
    assert_eq!(catalog.engine.tag_name, "launcher@0.2.38");
    assert_eq!(catalog.game.tag_name, "nightly");
    assert_eq!(
        catalog.game_version().unwrap(),
        "nightly-2026-11-04-67dad0d"
    );
    let stable = github::select_catalog(&all, Channel::Stable, "linux", "x86_64").unwrap();
    assert_eq!(stable.game_version().unwrap(), "v0.2.99");
}

#[test]
fn new_nightly_with_same_tag_is_detectable_by_digest_and_revision() {
    let old = github::select_catalog(&releases(), Channel::Nightly, "linux", "x86_64").unwrap();
    let mut new = old.clone();
    new.game.name = "Nightly 2026-11-04 · abcdef0".into();
    new.game_asset.digest = Some(format!("sha256:{}", github::digest_hex(b"next")));
    assert_ne!(old.game_version().unwrap(), new.game_version().unwrap());
    assert_ne!(
        mcrslauncher::engine::game_fingerprint(&old),
        mcrslauncher::engine::game_fingerprint(&new)
    );
}

#[test]
fn draft_engine_and_unsupported_platform_are_rejected() {
    let mut all = releases();
    all[0].draft = true;
    assert_eq!(
        github::select_catalog(&all, Channel::Nightly, "linux", "x86_64")
            .unwrap()
            .engine
            .tag_name,
        "launcher@0.2.37"
    );
    assert!(github::select_catalog(&all, Channel::Nightly, "linux", "aarch64").is_err());
    all.retain(|r| r.prerelease);
    assert!(github::select_catalog(&all, Channel::Stable, "linux", "x86_64").is_err());
}

#[test]
fn verifies_sha256_and_rejects_corruption_or_missing_algorithm() {
    let hash = format!("sha256:{}", github::digest_hex(b"authentic"));
    assert!(github::verify_digest(b"authentic", &hash).is_ok());
    assert!(github::verify_digest(b"corrupted", &hash).is_err());
    assert!(github::verify_digest(b"authentic", "sha1:abcd").is_err());
    assert!(github::verify_digest(b"authentic", "sha256:").is_err());
}

#[test]
fn mod_links_accept_pinned_sources_and_reject_untrusted_hosts_and_arguments() {
    for good in [
        "https://github.com/horizon-rust/example",
        "https://github.com/owner/repo/tree/main",
        "https://github.com/owner/repo/commit/abcdef0",
        "https://github.com/owner/repo/releases/tag/v1.0",
    ] {
        assert!(github::validate_mod_url(good).is_ok(), "{good}");
    }
    for bad in [
        "file:///tmp/repo",
        "http://github.com/owner/repo",
        "https://github.com.evil.test/owner/repo",
        "https://user:pass@github.com/owner/repo",
        "https://github.com/owner/repo?cmd=evil",
        "https://github.com/owner/repo/issues/3",
        "https://github.com/owner/repo/tree/x%3By",
        "--upload-pack=evil",
    ] {
        assert!(github::validate_mod_url(bad).is_err(), "{bad}");
    }
}

#[test]
fn device_code_parser_only_opens_microsoft_https_urls() {
    let line =
        "To sign in, open https://microsoft.com/link in a browser and enter the code ABCD1234";
    assert_eq!(
        parse_login(line),
        Some(("https://microsoft.com/link".into(), "ABCD1234".into()))
    );
    assert!(parse_login(&line.replace("microsoft.com", "evil.test")).is_none());
    assert!(parse_login(&line.replace("https:", "http:")).is_none());
    assert!(parse_login(&line.replace("ABCD1234", "$(execute)")).is_none());
}

#[test]
fn settings_are_atomic_and_corruption_is_reported() {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths {
        root: temp.path().to_path_buf(),
    };
    let mut settings = Settings::default();
    paths.save_settings(&settings).unwrap();
    settings.update_before_play = false;
    paths.save_settings(&settings).unwrap();
    assert!(!paths.load_settings().unwrap().update_before_play);
    fs::write(paths.config(), "{broken").unwrap();
    assert!(paths.load_settings().is_err());
}

#[test]
fn archive_extraction_ignores_other_files_and_replaces_only_the_engine() {
    use std::io::Write;
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join(if cfg!(windows) {
        "horizon-launcher.exe"
    } else {
        "horizon-launcher"
    });
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("../../unrelated", options).unwrap();
    zip.write_all(b"must not be extracted").unwrap();
    zip.start_file(
        format!(
            "package/{}",
            destination.file_name().unwrap().to_str().unwrap()
        ),
        options,
    )
    .unwrap();
    zip.write_all(b"the program").unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    github::extract_engine(&bytes, "release.zip", &destination).unwrap();
    assert_eq!(fs::read(&destination).unwrap(), b"the program");
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    assert!(github::extract_engine(b"invalid", "broken.zip", &destination).is_err());
    assert_eq!(fs::read(&destination).unwrap(), b"the program");
}

#[cfg(unix)]
fn fake_engine(script: &str) -> (tempfile::TempDir, Engine) {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths {
        root: temp.path().to_path_buf(),
    };
    paths.create().unwrap();
    fs::write(paths.engine(), format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(paths.engine(), fs::Permissions::from_mode(0o755)).unwrap();
    let engine = Engine::new(paths, Arc::new(AtomicBool::new(false))).unwrap();
    let receipt = mcrslauncher::model::Receipt {
        executable_sha256: github::digest_hex(&fs::read(engine.paths.engine()).unwrap()),
        ..Default::default()
    };
    fs::write(
        engine.paths.receipt(),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    (temp, engine)
}

#[cfg(unix)]
fn fake_build_engine(
    script: &str,
    selected: &str,
    installed: &[&str],
    prepared: &[&str],
) -> (tempfile::TempDir, Engine) {
    let versions = serde_json::json!({"installed":installed,"selected":selected});
    let prefix = format!(
        r#"if [ "$1" = --json ]; then
    case "$2" in
        account) printf '%s\n' '{{"accounts":[],"owner":true,"sign_in_configured":true}}';;
        versions) printf '%s\n' '{versions}';;
        instance) printf '[]\n';;
    esac
    exit 0
fi
"#
    );
    let (directory, engine) = fake_engine(&(prefix + script));
    for version in prepared {
        let base = engine.paths.engine_home().join("versions").join(version);
        let resources = base.join(".local/horizon-client/26.3");
        fs::create_dir_all(resources.join("assets/minecraft")).unwrap();
        fs::write(
            base.join("research-lock.json"),
            r#"{"minecraft":{"version":"26.3"}}"#,
        )
        .unwrap();
        fs::write(resources.join("horizon-resources.json"), "{}").unwrap();
        fs::write(resources.join("assets/minecraft/sounds.json"), "{}").unwrap();
    }
    (directory, engine)
}

#[cfg(unix)]
#[test]
fn process_arguments_are_not_interpreted_by_a_shell() {
    let (_temp, engine) = fake_engine("printf '%s\\n' \"$@\"");
    let payload = "hello; $(touch /tmp/mcrs-should-never-exist)";
    let output = engine.run(&[payload], &|_| {}, true).unwrap();
    assert_eq!(output.trim(), payload);
}

#[cfg(unix)]
#[test]
fn cancellation_stops_the_entire_process_tree() {
    let (_temp, engine) = fake_engine("sleep 60 &\nwait");
    let cancel = engine.cancel.clone();
    let started = Instant::now();
    let signal = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        cancel.store(true, Ordering::Relaxed);
    });
    assert!(
        engine
            .run(&["test"], &|_| {}, true)
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    signal.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[cfg(unix)]
#[test]
fn game_output_memory_stays_bounded() {
    let (_temp, engine) = fake_engine(
        "i=0; while [ $i -lt 5000 ]; do printf 'A deliberately long log line to test the bounded process log capture.\\n'; i=$((i+1)); done",
    );
    let output = engine.run(&[], &|_| {}, true).unwrap();
    assert!(output.len() <= 128 * 1024);
    assert!(!output.is_empty());
}

#[cfg(unix)]
#[test]
fn non_utf8_process_diagnostics_do_not_break_the_launcher() {
    let (_temp, engine) = fake_engine("printf '\\377\\376\\n'");
    let output = engine.run(&[], &|_| {}, true).unwrap();
    assert!(output.contains(char::REPLACEMENT_CHARACTER));
}

#[cfg(unix)]
#[test]
fn failed_native_build_remains_pending_for_retry() {
    let (_temp, engine) = fake_build_engine(
        "echo 'compiler failed' >&2; exit 1",
        "v0.2.97",
        &["v0.2.97"],
        &["v0.2.97"],
    );
    let error = engine
        .build(&Settings::default(), false, &|_| {})
        .unwrap_err();
    assert!(error.to_string().contains("compiler failed"));
    assert!(engine.receipt().mods_dirty);
}

#[cfg(unix)]
#[test]
fn native_build_uses_requested_profile_and_clears_pending_changes_on_success() {
    let (_temp, engine) = fake_build_engine(
        "printf '%s\\n' \"$@\" > \"$HORIZON_LAUNCHER_HOME/arguments\"",
        "v0.2.97",
        &["v0.2.97"],
        &["v0.2.97"],
    );
    let settings = Settings {
        build_profile: mcrslauncher::model::BuildProfile::Release,
        ..Default::default()
    };
    engine.build(&settings, true, &|_| {}).unwrap();
    let arguments = fs::read_to_string(engine.paths.engine_home().join("arguments")).unwrap();
    assert!(arguments.contains("--profile\nrelease\n"));
    assert!(arguments.contains("--trust-native-code\n"));
    assert!(arguments.contains("--version\nv0.2.97\n"));
    assert!(!arguments.contains("--update\n"));
    assert!(!engine.receipt().mods_dirty);
}

#[cfg(unix)]
#[test]
fn native_build_prepares_the_new_base_before_compiling_and_leaves_selection_to_upstream() {
    let script = r#"case "$1" in
prepare)
    printf '%s' "$4" > "$HORIZON_LAUNCHER_HOME/prepared"
    printf 'prepare\n' >> "$HORIZON_LAUNCHER_HOME/order";;
native-build)
    test "$2" = --version || exit 3
    test "$(cat "$HORIZON_LAUNCHER_HOME/prepared")" = "$3" || exit 4
    printf '%s\n' "$@" > "$HORIZON_LAUNCHER_HOME/arguments"
    printf 'build\n' >> "$HORIZON_LAUNCHER_HOME/order";;
*) exit 5;;
esac"#;
    let (_directory, engine) = fake_build_engine(
        script,
        "native-old-cache",
        &["nightly-new", "native-old-cache", "v0.2.97"],
        &["native-old-cache", "v0.2.97"],
    );
    engine.build(&Settings::default(), true, &|_| {}).unwrap();
    let home = engine.paths.engine_home();
    assert_eq!(
        fs::read_to_string(home.join("order")).unwrap(),
        "prepare\nbuild\n"
    );
    let arguments = fs::read_to_string(home.join("arguments")).unwrap();
    assert!(arguments.contains("--version\nnightly-new\n"));
    assert!(!engine.receipt().mods_dirty);
}

#[cfg(unix)]
#[test]
fn rebuilding_a_prepared_native_version_keeps_its_revision_and_skips_preparation() {
    let script = r#"test "$1" = native-build || exit 3
printf '%s\n' "$@" > "$HORIZON_LAUNCHER_HOME/arguments""#;
    let (_directory, engine) = fake_build_engine(
        script,
        "native-old-cache",
        &["nightly-new", "native-old-cache", "v0.2.97"],
        &["native-old-cache"],
    );
    engine.build(&Settings::default(), false, &|_| {}).unwrap();
    let arguments = fs::read_to_string(engine.paths.engine_home().join("arguments")).unwrap();
    assert!(arguments.contains("--version\nnative-old-cache\n"));
}

#[cfg(unix)]
#[test]
fn failed_resource_preparation_blocks_compilation_and_preserves_pending_mod_changes() {
    let script = r#"printf '%s\n' "$1" >> "$HORIZON_LAUNCHER_HOME/order"
if [ "$1" = prepare ]; then printf 'error: resource exporter failed\n' >&2; exit 1; fi
exit 3"#;
    let (_directory, engine) = fake_build_engine(script, "nightly-new", &["nightly-new"], &[]);
    let error = engine
        .build(&Settings::default(), false, &|_| {})
        .unwrap_err();
    assert!(error.to_string().contains("resource exporter failed"));
    assert_eq!(
        fs::read_to_string(engine.paths.engine_home().join("order")).unwrap(),
        "prepare\n"
    );
    assert!(engine.receipt().mods_dirty);
}

#[cfg(unix)]
#[test]
fn game_install_builds_on_its_catalog_version_when_another_channel_is_installed() {
    let catalog = github::select_catalog(&releases(), Channel::Stable, "linux", "x86_64").unwrap();
    let script = r#"test "$1" = native-build || exit 3
printf '%s\n' "$@" > "$HORIZON_LAUNCHER_HOME/arguments""#;
    let (_directory, engine) = fake_build_engine(
        script,
        "nightly-new",
        &["nightly-new", "v0.2.99"],
        &["v0.2.99"],
    );
    let mods = engine.paths.game_home().join("horizon-native-mods");
    fs::create_dir_all(&mods).unwrap();
    fs::write(
        mods.join("test.origin.json"),
        r#"{"id":"test","owner":"example","repo":"test","commit":"abc"}"#,
    )
    .unwrap();
    let mut receipt = engine.receipt();
    receipt.tag = catalog.engine.tag_name.clone();
    receipt.digest = catalog.engine_asset.digest.clone().unwrap();
    receipt.game_tag = "v0.2.99".into();
    receipt.game_digest = mcrslauncher::engine::game_fingerprint(&catalog);
    fs::write(
        engine.paths.receipt(),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    engine
        .install_game(&catalog, &Settings::default(), true, &|_| {})
        .unwrap();
    let arguments = fs::read_to_string(engine.paths.engine_home().join("arguments")).unwrap();
    assert!(arguments.contains("--version\nv0.2.99\n"));
}

#[cfg(unix)]
#[test]
fn installing_a_new_nightly_removes_unpinned_old_nightlies() {
    let catalog = github::select_catalog(&releases(), Channel::Nightly, "linux", "x86_64").unwrap();
    let current = catalog.game_version().unwrap();
    let old = "nightly-2026-11-04-aaaaaaa";
    let pinned = "nightly-2026-11-03-bbbbbbb";
    let older = "nightly-2026-11-03-ccccccc";
    let native = "native-67dad0d-0123456789ab";
    let script = format!(
        r#"if [ "$1" = --json ]; then
    case "$2" in
        account) printf '%s\n' '{{"accounts":[],"owner":true,"sign_in_configured":true}}';;
        versions) printf '%s\n' '{{"installed":["{current}","{old}","{pinned}","{older}","v0.2.97","{native}"],"selected":"{native}"}}';;
        instance) printf '%s\n' '[{{"id":"main","selected":true,"version":"{pinned}"}}]';;
        settings) printf '%s\n' '{{"selected_version":"{native}"}}';;
    esac
    exit 0
fi
printf '%s\n' "$@" >> "$HORIZON_LAUNCHER_HOME/commands"
"#
    );
    let (_directory, engine) = fake_engine(&script);
    let mut receipt = engine.receipt();
    receipt.tag = catalog.engine.tag_name.clone();
    receipt.digest = catalog.engine_asset.digest.clone().unwrap();
    receipt.game_tag = old.into();
    receipt.game_digest = "old-digest".into();
    fs::write(
        engine.paths.receipt(),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    engine
        .install_game(&catalog, &Settings::default(), true, &|_| {})
        .unwrap();
    assert_eq!(
        fs::read_to_string(engine.paths.engine_home().join("commands")).unwrap(),
        format!(
            "install\n{current}\nsettings\nselected-version\n{current}\n\
             remove\n{old}\nremove\n{older}\n"
        )
    );
}

#[cfg(unix)]
#[test]
fn snapshot_preserves_the_selected_game_folder_and_public_skin_metadata() {
    let script = r#"case "$2" in
account) printf '%s\n' '{"accounts":[{"name":"Player","uuid":"0123456789abcdef0123456789abcdef","selected":true}],"sign_in_configured":true,"owner":true}';;
versions) printf '%s\n' '{"installed":["test"],"selected":"test"}';;
instance) printf '[{"id":"main","selected":true,"home":"%s/selected"}]\n' "$HORIZON_HOME";;
esac"#;
    let (_directory, engine) = fake_engine(script);
    fs::write(engine.paths.engine_home().join("accounts.json"), serde_json::json!({"accounts":[{"uuid":"0123456789abcdef0123456789abcdef","name":"Player","skin":["https://textures.minecraft.net/texture/abcdef","slim"]}]}).to_string()).unwrap();
    let snapshot = engine.snapshot().unwrap();
    assert_eq!(
        snapshot.game_home,
        engine.paths.game_home().join("selected")
    );
    assert_eq!(snapshot.accounts[0].skin.as_ref().unwrap().1, "slim");
}
