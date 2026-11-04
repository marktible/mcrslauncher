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
    let (_temp, engine) = fake_engine("echo 'compiler failed' >&2; exit 1");
    let error = engine
        .build(&Settings::default(), false, &|_| {})
        .unwrap_err();
    assert!(error.to_string().contains("compiler failed"));
    assert!(engine.receipt().mods_dirty);
}

#[cfg(unix)]
#[test]
fn native_build_uses_requested_profile_and_clears_pending_changes_on_success() {
    let (_temp, engine) =
        fake_engine("printf '%s\\n' \"$@\" > \"$HORIZON_LAUNCHER_HOME/arguments\"");
    let settings = Settings {
        build_profile: mcrslauncher::model::BuildProfile::Release,
        ..Default::default()
    };
    engine.build(&settings, true, &|_| {}).unwrap();
    let arguments = fs::read_to_string(engine.paths.engine_home().join("arguments")).unwrap();
    assert!(arguments.contains("--profile\nrelease\n"));
    assert!(arguments.contains("--trust-native-code\n"));
    assert!(arguments.contains("--update\n"));
    assert!(!engine.receipt().mods_dirty);
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
