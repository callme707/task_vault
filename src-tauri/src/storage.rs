//! Small, fail-closed storage primitives shared by the vault and its settings.
//!
//! Secrets are protected with the Windows current-user DPAPI.  The envelope is
//! deliberately versioned so that a file with an unknown format cannot be
//! passed to the platform API accidentally.  There is no plaintext fallback on
//! non-Windows platforms.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const ENVELOPE_MAGIC: &[u8] = b"TVAULT-DPAPI";
const ENVELOPE_VERSION: u8 = 1;
const ENVELOPE_HEADER_LEN: usize = ENVELOPE_MAGIC.len() + 1;
#[cfg(windows)]
const MAX_DPAPI_SIZE: usize = u32::MAX as usize;

/// Returns whether `bytes` has this module's current protected envelope.
///
/// This is a format check, not a proof that the ciphertext can be decrypted.
/// Call [`unprotect`] to authenticate and decrypt the payload.
pub fn is_protected(bytes: &[u8]) -> bool {
    bytes.len() > ENVELOPE_HEADER_LEN
        && bytes.starts_with(ENVELOPE_MAGIC)
        && bytes[ENVELOPE_MAGIC.len()] == ENVELOPE_VERSION
}

/// Protect bytes with the current Windows user's DPAPI key.
pub fn protect(bytes: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    {
        let protected = protect_windows(bytes)?;
        let mut envelope = Vec::with_capacity(ENVELOPE_HEADER_LEN + protected.len());
        envelope.extend_from_slice(ENVELOPE_MAGIC);
        envelope.push(ENVELOPE_VERSION);
        envelope.extend_from_slice(&protected);
        Ok(envelope)
    }

    #[cfg(not(windows))]
    {
        let _ = bytes;
        Err("DPAPI protected storage is only available on Windows".to_string())
    }
}

/// Authenticate and decrypt a current-user DPAPI envelope.
pub fn unprotect(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if !is_protected(bytes) {
        return Err("invalid or unsupported protected storage envelope".to_string());
    }

    #[cfg(windows)]
    {
        unprotect_windows(&bytes[ENVELOPE_HEADER_LEN..])
    }

    #[cfg(not(windows))]
    {
        let _ = bytes;
        Err("DPAPI protected storage is only available on Windows".to_string())
    }
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically replace `path` with `bytes`.
///
/// The temporary file is created with `create_new` in the destination's
/// directory, fully written and flushed before the replacement.  On Windows,
/// `MoveFileExW` provides the replace/write-through operation.  The old
/// destination is untouched if writing or replacement fails.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if path.file_name().is_none() {
        return Err(format!("invalid destination path: {}", path.display()));
    }

    let (temporary_path, mut temporary_file) = create_unique_temp(parent)?;
    let result = (|| {
        temporary_file
            .write_all(bytes)
            .map_err(|e| format!("write temporary file failed: {e}"))?;
        temporary_file
            .sync_all()
            .map_err(|e| format!("flush temporary file failed: {e}"))?;
        drop(temporary_file);
        replace_file(&temporary_path, path)
    })();

    if result.is_err() {
        // The replacement has not happened when this branch runs.  Best-effort
        // cleanup also avoids leaving protected material in a temporary file.
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn create_unique_temp(parent: &Path) -> Result<(PathBuf, File), String> {
    for _ in 0..128 {
        let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!(".taskvault-{}-{}.tmp", std::process::id(), sequence);
        let candidate = parent.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "create temporary file {} failed: {error}",
                    candidate.display()
                ));
            }
        }
    }

    Err("could not allocate a unique temporary file".to_string())
}

#[cfg(windows)]
fn replace_file(temporary_path: &Path, destination: &Path) -> Result<(), String> {
    let temporary_path = wide_path(temporary_path)?;
    let destination_display = destination.display().to_string();
    let destination = wide_path(destination)?;
    let ok = unsafe {
        MoveFileExW(
            temporary_path.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        return Err(format!(
            "atomic replace {destination_display} failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(temporary_path: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(temporary_path, destination)
        .map_err(|e| format!("atomic replace {} failed: {e}", destination.display()))
}

#[cfg(windows)]
const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
#[cfg(windows)]
const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

#[cfg(windows)]
#[repr(C)]
struct DataBlob {
    cb_data: u32,
    pb_data: *mut u8,
}

#[cfg(windows)]
#[link(name = "crypt32")]
extern "system" {
    fn CryptProtectData(
        data_in: *const DataBlob,
        data_description: *const u16,
        optional_entropy: *const DataBlob,
        reserved: *mut std::ffi::c_void,
        prompt_struct: *const std::ffi::c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;

    fn CryptUnprotectData(
        data_in: *const DataBlob,
        data_description: *mut *mut u16,
        optional_entropy: *const DataBlob,
        reserved: *mut std::ffi::c_void,
        prompt_struct: *const std::ffi::c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

#[cfg(windows)]
const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x0000_0001;

#[cfg(windows)]
fn protect_windows(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let input = data_blob(bytes)?;
    let mut output = DataBlob {
        cb_data: 0,
        pb_data: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        if !output.pb_data.is_null() {
            unsafe { LocalFree(output.pb_data.cast()) };
        }
        return Err(format!(
            "DPAPI protect failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    copy_and_free_blob(output, "DPAPI protect")
}

#[cfg(windows)]
fn unprotect_windows(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.is_empty() {
        return Err("protected storage envelope has no ciphertext".to_string());
    }
    let input = data_blob(bytes)?;
    let mut output = DataBlob {
        cb_data: 0,
        pb_data: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        if !output.pb_data.is_null() {
            unsafe { LocalFree(output.pb_data.cast()) };
        }
        return Err(format!(
            "DPAPI unprotect failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    copy_and_free_blob(output, "DPAPI unprotect")
}

#[cfg(windows)]
fn data_blob(bytes: &[u8]) -> Result<DataBlob, String> {
    let cb_data = u32::try_from(bytes.len())
        .map_err(|_| format!("DPAPI input is too large (maximum {MAX_DPAPI_SIZE} bytes)"))?;
    Ok(DataBlob {
        cb_data,
        pb_data: if bytes.is_empty() {
            std::ptr::null_mut()
        } else {
            bytes.as_ptr() as *mut u8
        },
    })
}

#[cfg(windows)]
fn copy_and_free_blob(blob: DataBlob, operation: &str) -> Result<Vec<u8>, String> {
    if blob.cb_data == 0 || blob.pb_data.is_null() {
        if !blob.pb_data.is_null() {
            unsafe { LocalFree(blob.pb_data.cast()) };
        }
        return Err(format!("{operation} returned an empty buffer"));
    }
    let bytes = unsafe {
        std::slice::from_raw_parts(blob.pb_data as *const u8, blob.cb_data as usize).to_vec()
    };
    unsafe { LocalFree(blob.pb_data.cast()) };
    Ok(bytes)
}

#[cfg(windows)]
fn wide_path(path: &Path) -> Result<Vec<u16>, String> {
    use std::os::windows::ffi::OsStrExt;

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(format!("path contains an embedded NUL: {}", path.display()));
    }
    wide.push(0);
    Ok(wide)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "taskvault-storage-test-{}-{}-{}",
                std::process::id(),
                label,
                id
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let Ok(temp_root) = fs::canonicalize(std::env::temp_dir()) else {
                return;
            };
            let Ok(candidate) = fs::canonicalize(&self.0) else {
                return;
            };
            let safe_name = candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("taskvault-storage-test-"));
            if candidate.starts_with(temp_root) && safe_name {
                let _ = fs::remove_dir_all(candidate);
            }
        }
    }

    #[test]
    fn atomic_write_replaces_file() {
        let dir = TestDir::new("replace");
        let path = dir.path().join("value.json");
        fs::write(&path, b"old").expect("seed destination");

        atomic_write(&path, b"new").expect("atomic write");
        assert_eq!(fs::read(&path).expect("read destination"), b"new");
    }

    #[test]
    fn failed_replace_preserves_existing_destination() {
        let dir = TestDir::new("preserve");
        let destination = dir.path().join("destination");
        fs::create_dir(&destination).expect("seed directory destination");

        let result = atomic_write(&destination, b"new");
        assert!(result.is_err());
        assert!(destination.is_dir());
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_roundtrip_and_corruption_checks() {
        let plaintext = b"taskvault secret bytes";
        let protected = protect(plaintext).expect("protect");
        assert!(is_protected(&protected));
        assert_eq!(unprotect(&protected).expect("unprotect"), plaintext);

        let mut wrong_magic = protected.clone();
        wrong_magic[0] ^= 1;
        assert!(!is_protected(&wrong_magic));
        assert!(unprotect(&wrong_magic).is_err());

        let mut wrong_version = protected.clone();
        wrong_version[ENVELOPE_MAGIC.len()] = ENVELOPE_VERSION + 1;
        assert!(!is_protected(&wrong_version));
        assert!(unprotect(&wrong_version).is_err());

        let mut corrupt_payload = protected;
        let last = corrupt_payload.len() - 1;
        corrupt_payload[last] ^= 0x5a;
        assert!(is_protected(&corrupt_payload));
        assert!(unprotect(&corrupt_payload).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn dpapi_fails_closed_off_windows() {
        assert!(protect(b"secret").is_err());
        assert!(unprotect(&[ENVELOPE_MAGIC, &[ENVELOPE_VERSION], b"ciphertext"].concat()).is_err());
    }
}
