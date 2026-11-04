use std::{
    path::PathBuf,
    time::{Duration, SystemTime},
};

#[derive(Clone, Debug)]
pub struct TransferProgress {
    pub downloaded: u64,
    pub total: u64,
    pub bytes_per_second: f64,
}

impl TransferProgress {
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.
        } else {
            (self.downloaded as f64 / self.total as f64).clamp(0., 1.) as f32
        }
    }

    pub fn description(&self) -> String {
        let mut text = format!(
            "{:.0}% · {} / {}",
            self.fraction() * 100.,
            format_bytes(self.downloaded),
            format_bytes(self.total)
        );
        if self.bytes_per_second >= 1. {
            text.push_str(&format!(
                " · {}/s",
                format_bytes(self.bytes_per_second as u64)
            ));
            if self.downloaded < self.total {
                let eta =
                    ((self.total - self.downloaded) as f64 / self.bytes_per_second).ceil() as u64;
                text.push_str(&format!(
                    " · {} left",
                    format_duration(Duration::from_secs(eta))
                ));
            }
        }
        if self.bytes_per_second < 1. {
            text.push_str(" · 0 B/s");
        }
        text
    }
}

#[derive(Default)]
pub struct TransferMeter {
    previous: Option<(u64, Duration)>,
    rate: f64,
    last_advance: Duration,
}

impl TransferMeter {
    pub fn observe(&mut self, downloaded: u64, total: u64, elapsed: Duration) -> TransferProgress {
        match self.previous {
            Some((bytes, _)) if downloaded < bytes => {
                self.rate = 0.;
                self.last_advance = elapsed;
                self.previous = Some((downloaded, elapsed));
            }
            Some((bytes, time)) if elapsed.saturating_sub(time) >= Duration::from_millis(250) => {
                if downloaded > bytes {
                    self.last_advance = elapsed;
                }
                let rate = downloaded.saturating_sub(bytes) as f64
                    / elapsed.saturating_sub(time).as_secs_f64();
                self.rate = if self.rate == 0. {
                    rate
                } else {
                    self.rate * 0.6 + rate * 0.4
                };
                self.previous = Some((downloaded, elapsed));
            }
            None => {
                self.previous = Some((downloaded, elapsed));
                self.last_advance = elapsed;
            }
            _ => {}
        }
        if elapsed.saturating_sub(self.last_advance) >= Duration::from_secs(3) {
            self.rate = 0.;
        }
        TransferProgress {
            downloaded: downloaded.min(total),
            total,
            bytes_per_second: self.rate,
        }
    }
}

pub struct FileTransfer {
    pub path: PathBuf,
    pub total: u64,
    meter: TransferMeter,
    seen: bool,
    finished: bool,
    initial: Option<(u64, Option<SystemTime>)>,
}

pub enum FileProgress {
    Download(TransferProgress),
    Unpacking,
}

impl FileTransfer {
    pub fn new(path: PathBuf, total: u64) -> Self {
        let initial = std::fs::metadata(&path)
            .ok()
            .map(|m| (m.len(), m.modified().ok()));
        Self {
            path,
            total,
            meter: TransferMeter::default(),
            seen: false,
            finished: false,
            initial,
        }
    }

    pub fn poll(&mut self, elapsed: Duration) -> Option<FileProgress> {
        if self.finished {
            return None;
        }
        let metadata = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(_) => {
                self.initial = None;
                return None;
            }
        };
        let bytes = metadata.len();
        if self.initial == Some((bytes, metadata.modified().ok())) {
            return None;
        }
        self.initial = None;
        if self.seen && self.total > 0 && bytes >= self.total {
            self.finished = true;
            return Some(FileProgress::Unpacking);
        }
        self.seen = true;
        Some(FileProgress::Download(
            self.meter.observe(bytes, self.total, elapsed),
        ))
    }
}

pub fn format_bytes(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KiB", bytes as f64 / 1024.),
        1_048_576..1_073_741_824 => format!("{:.1} MiB", bytes as f64 / 1_048_576.),
        _ => format!("{:.2} GiB", bytes as f64 / 1_073_741_824.),
    }
}

pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, seconds / 60 % 60)
    } else if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}
