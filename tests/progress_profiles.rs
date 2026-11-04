use mcrslauncher::{
    model::Paths,
    profiles::{consume_session, normalize_uuid, skin_head, texture_url},
    progress::{FileProgress, FileTransfer, TransferMeter, TransferProgress},
};
use std::{fs, io::Cursor, time::Duration};

#[test]
fn measures_download_bytes_speed_and_eta() {
    let mut meter = TransferMeter::default();
    meter.observe(0, 200, Duration::ZERO);
    let progress = meter.observe(100, 200, Duration::from_secs(2));
    assert_eq!(progress.fraction(), 0.5);
    assert_eq!(progress.bytes_per_second, 50.);
    assert!(progress.description().contains("2s left"));
    let reset = meter.observe(10, 200, Duration::from_secs(3));
    assert_eq!(reset.bytes_per_second, 0.);
    assert_eq!(reset.downloaded, 10);
}

#[test]
fn unknown_totals_and_excess_bytes_do_not_produce_invalid_percentages() {
    let progress = TransferProgress {
        downloaded: 10,
        total: 0,
        bytes_per_second: 0.,
    };
    assert_eq!(progress.fraction(), 0.);
    let progress = TransferProgress {
        downloaded: 500,
        total: 100,
        bytes_per_second: 10.,
    };
    assert_eq!(progress.fraction(), 1.);
    assert!(!progress.description().contains("left"));
}

#[test]
fn follows_actual_archive_growth_and_then_reports_unpacking() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("download");
    let mut transfer = FileTransfer::new(path.clone(), 10);
    assert!(transfer.poll(Duration::ZERO).is_none());
    fs::write(&path, b"12345").unwrap();
    let Some(FileProgress::Download(progress)) = transfer.poll(Duration::from_secs(1)) else {
        panic!("missing download progress");
    };
    assert_eq!(progress.fraction(), 0.5);
    fs::write(&path, b"1234567890").unwrap();
    assert!(matches!(
        transfer.poll(Duration::from_secs(2)),
        Some(FileProgress::Unpacking)
    ));
    assert!(transfer.poll(Duration::from_secs(3)).is_none());
}

#[test]
fn ignores_previous_partial_downloads_until_the_current_install_writes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("download");
    fs::write(&path, b"previous download").unwrap();
    let mut transfer = FileTransfer::new(path.clone(), 20);
    assert!(transfer.poll(Duration::from_secs(1)).is_none());
    fs::write(&path, b"current").unwrap();
    let Some(FileProgress::Download(progress)) = transfer.poll(Duration::from_secs(2)) else {
        panic!("missing current progress");
    };
    assert_eq!(progress.downloaded, 7);
}

#[test]
fn stalled_downloads_stop_showing_an_outdated_eta() {
    let mut meter = TransferMeter::default();
    meter.observe(0, 200, Duration::ZERO);
    meter.observe(100, 200, Duration::from_secs(1));
    let stalled = meter.observe(100, 200, Duration::from_secs(5));
    assert_eq!(stalled.bytes_per_second, 0.);
    assert!(!stalled.description().contains("left"));
}

fn png(image: image::RgbaImage) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

#[test]
fn renders_real_face_pixels_with_transparent_hat_overlay() {
    let mut image = image::RgbaImage::new(64, 64);
    image.put_pixel(8, 8, image::Rgba([100, 50, 0, 255]));
    image.put_pixel(40, 8, image::Rgba([0, 0, 200, 128]));
    image.put_pixel(9, 8, image::Rgba([10, 20, 30, 255]));
    let head = skin_head(&png(image)).unwrap();
    assert_eq!(head.len(), 8 * 8 * 4);
    assert_eq!(&head[..4], &[50, 25, 100, 255]);
    assert_eq!(&head[4..8], &[10, 20, 30, 255]);
}

#[test]
fn accepts_legacy_png_skins_and_rejects_other_dimensions_or_files() {
    assert!(skin_head(&png(image::RgbaImage::new(64, 32))).is_ok());
    assert!(skin_head(&png(image::RgbaImage::new(128, 128))).is_err());
    assert!(skin_head(b"not a PNG").is_err());
}

#[test]
fn only_fetches_official_skin_textures_over_https() {
    assert_eq!(
        texture_url("http://textures.minecraft.net/texture/abcdef12")
            .unwrap()
            .scheme(),
        "https"
    );
    for invalid in [
        "https://evil.test/texture/abcdef",
        "https://textures.minecraft.net.evil.test/texture/abcdef",
        "https://textures.minecraft.net/texture/../other",
        "https://user@textures.minecraft.net/texture/abcdef",
        "https://textures.minecraft.net/texture/abcd?token=secret",
    ] {
        assert!(texture_url(invalid).is_err(), "{invalid}");
    }
    assert!(normalize_uuid("../../secrets").is_err());
}

const UUID: &str = "0123456789abcdef0123456789abcdef";

#[test]
fn consumes_a_matching_read_once_session_without_persisting_the_token() {
    let directory = tempfile::tempdir().unwrap();
    let paths = Paths {
        root: directory.path().to_path_buf(),
    };
    let sessions = paths.game_home().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let file = sessions.join("session-test.json");
    fs::write(
        &file,
        serde_json::json!({"uuid":UUID,"access_token":"test-token"}).to_string(),
    )
    .unwrap();
    let token = consume_session(&paths, &file, UUID).unwrap();
    assert_eq!(token.as_str(), "test-token");
    assert!(!file.exists());
}

#[test]
fn rejects_wrong_accounts_and_malformed_sessions_and_removes_the_private_file() {
    let directory = tempfile::tempdir().unwrap();
    let paths = Paths {
        root: directory.path().to_path_buf(),
    };
    let sessions = paths.game_home().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    for content in [
        serde_json::json!({"uuid":"ffffffffffffffffffffffffffffffff","access_token":"test-token"})
            .to_string(),
        "invalid session".into(),
    ] {
        let file = sessions.join("session-test.json");
        fs::write(&file, content).unwrap();
        assert!(consume_session(&paths, &file, UUID).is_err());
        assert!(!file.exists());
    }
    let outside = directory.path().join("unrelated.json");
    fs::write(&outside, "do not consume").unwrap();
    assert!(consume_session(&paths, &outside, UUID).is_err());
    assert!(outside.exists());
}
