//! Runs a copy of the current launcher outside the installation directory so
//! Windows can release the installed executable before it is replaced.
use super::*;
use std::{ffi::OsString, os::windows::process::CommandExt, process::Command, thread};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBoxW(
        window: *mut std::ffi::c_void,
        text: *const u16,
        caption: *const u16,
        flags: u32,
    ) -> i32;
}

pub fn start_helper(binary: &Path, destination: &Path) -> Result<()> {
    let ready = binary.with_file_name("helper-ready");
    let _ = fs::remove_file(&ready);
    let mut child = Command::new(binary.with_file_name("mcrs-update-helper.exe"))
        .arg("--apply-launcher-update")
        .arg(std::process::id().to_string())
        .arg(binary)
        .arg(destination)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .spawn()
        .context("Could not start the Windows update helper")?;
    // The helper opens the parent handle before acknowledging readiness. Never
    // close the GUI if the helper failed to start or cannot wait for this process.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if ready.is_file() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("The Windows update helper exited: {status}");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("The Windows update helper did not become ready. Try again.");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

/// Called before GUI initialization. All internal paths use OsString, allowing
/// Windows usernames and installation folders containing non-ASCII characters.
pub fn run_if_requested(args: &[OsString]) -> Option<i32> {
    if args
        .first()
        .is_none_or(|arg| arg != "--apply-launcher-update")
    {
        return None;
    }
    let result = apply(args);
    if let Err(error) = &result {
        let message = format!(
            "MCRS could not finish updating. Your downloaded update is retained.\n\n{error:#}"
        );
        if let Ok(exe) = std::env::current_exe() {
            let _ = fs::write(exe.with_file_name("update-error.txt"), &message);
        }
        let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
        let title: Vec<u16> = "MCRS Launcher update"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: both strings are NUL terminated and live for the entire call.
        unsafe {
            MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0x10);
        }
    }
    Some(if result.is_ok() { 0 } else { 1 })
}

fn apply(args: &[OsString]) -> Result<()> {
    ensure!(args.len() == 4, "Invalid update helper arguments");
    let pid: u32 = args[1]
        .to_str()
        .context("Invalid parent process")?
        .parse()?;
    let binary = Path::new(&args[2]);
    let destination = Path::new(&args[3]);
    ensure!(
        binary.is_file() && destination.is_absolute(),
        "Invalid update paths"
    );
    // SYNCHRONIZE is the only access right required to wait on a process.
    // SAFETY: OpenProcess takes scalar arguments and returns an owned handle.
    let parent = unsafe { OpenProcess(0x00100000, 0, pid) };
    ensure!(
        !parent.is_null(),
        "Cannot wait for the launcher: {}",
        std::io::Error::last_os_error()
    );
    let wait_result = (|| -> Result<()> {
        fs::write(binary.with_file_name("helper-ready"), b"ready")?;
        // SAFETY: parent is a valid process handle owned by this function.
        let status = unsafe { WaitForSingleObject(parent, 300_000) };
        ensure!(
            status == 0,
            "The launcher did not exit within five minutes (status {status})"
        );
        Ok(())
    })();
    // SAFETY: close the process handle exactly once, after the wait completes.
    unsafe {
        CloseHandle(parent);
    }
    wait_result?;
    apply_and_restart(binary, destination)
}

fn apply_and_restart(binary: &Path, destination: &Path) -> Result<()> {
    let backup = binary.with_file_name("previous-launcher.exe");
    fs::copy(destination, &backup).context("Could not preserve the previous launcher")?;
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match replace_binary(binary, destination, &cancel) {
            Ok(()) => break,
            Err(error) if Instant::now() >= deadline => {
                return Err(error).context(
                    "Could not replace the launcher; close other launcher windows and retry",
                );
            }
            Err(_) => thread::sleep(Duration::from_millis(250)),
        }
    }
    if let Err(error) = Command::new(destination)
        .args(["--page", "updates"])
        .spawn()
    {
        replace_binary(&backup, destination, &cancel)
            .context("Could not restore the previous launcher")?;
        let _ = Command::new(destination)
            .args(["--page", "updates"])
            .spawn();
        return Err(error)
            .context("The updated launcher could not start; the previous version was restored");
    }
    // The helper executable itself remains locked until this process exits.
    // Retain it in the update cache; remove the large payload and backup now.
    let _ = fs::remove_file(binary);
    let _ = fs::remove_file(backup);
    let _ = fs::remove_file(binary.with_file_name("helper-ready"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_for_process_exit_before_replacing_and_restarting() {
        let folder = tempfile::tempdir().unwrap();
        let binary = folder.path().join("new launcher.exe");
        let destination = folder.path().join("installed launcher.exe");
        fs::write(&destination, b"previous executable").unwrap();
        fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
        fs::copy(
            std::env::current_exe().unwrap(),
            binary.with_file_name("mcrs-update-helper.exe"),
        )
        .unwrap();
        // Exercise the real wait primitive with a short-lived child process.
        let mut parent = Command::new("cmd.exe")
            .args(["/C", "ping -n 3 127.0.0.1 >nul"])
            .creation_flags(0x08000000)
            .spawn()
            .unwrap();
        let args = vec![
            OsString::from("--apply-launcher-update"),
            parent.id().to_string().into(),
            binary.clone().into_os_string(),
            destination.clone().into_os_string(),
        ];
        let job = thread::spawn(move || apply(&args));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !binary.with_file_name("helper-ready").exists() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fs::read(&destination).unwrap(), b"previous executable");
        parent.wait().unwrap();
        job.join().unwrap().unwrap();
        assert!(fs::metadata(&destination).unwrap().len() > 1000);
    }

    #[test]
    fn invalid_payload_preserves_previous_launcher() {
        let folder = tempfile::tempdir().unwrap();
        let binary = folder.path().join("invalid.exe");
        let destination = folder.path().join("installed.exe");
        fs::write(&destination, b"previous executable").unwrap();
        fs::write(&binary, b"invalid executable").unwrap();
        assert!(apply_and_restart(&binary, &destination).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"previous executable");
    }
}
