//! Validated, current-user encrypted image storage. No plaintext staging files.
use std::path::{Component, Path, PathBuf};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::deepseek::{encode_image, MAX_IMAGE_BYTES};
use crate::storage;

pub fn decode_capture(data: &str) -> Result<Vec<u8>, String> {
    let data = data.trim();
    let raw = if data.starts_with("data:") {
        let (header, body) = data.split_once(',').ok_or("图片数据格式不正确")?;
        if !header.starts_with("data:image/") || !header.ends_with(";base64") {
            return Err("请选择 Base64 编码的图片".into());
        }
        body
    } else {
        data
    };
    if raw.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
        return Err("图片超过 4 MB，请压缩后重试".into());
    }
    let bytes = B64.decode(raw).map_err(|_| "图片 Base64 数据无效")?;
    encode_image(&bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

pub fn safe_path(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let mut components = Path::new(name).components();
    if name.is_empty()
        || name.contains([':', '/', '\\'])
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err("附件路径无效".into());
    }
    let path = dir.join(name);
    if path.exists() {
        let root = dir.canonicalize().map_err(|e| e.to_string())?;
        let resolved = path.canonicalize().map_err(|e| e.to_string())?;
        if !resolved.starts_with(root) {
            return Err("附件路径超出存储目录".into());
        }
    }
    Ok(path)
}

pub fn save(dir: &Path, bytes: &[u8]) -> Result<String, String> {
    encode_image(bytes).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = format!("{:x}.tvimg", Sha256::digest(bytes));
    let path = safe_path(dir, &name)?;
    if path.exists() {
        let existing = read(dir, &name)?;
        if existing != bytes {
            return Err("附件内容校验失败".into());
        }
    } else {
        storage::atomic_write(&path, &storage::protect(bytes)?)?;
    }
    Ok(name)
}

pub fn read(dir: &Path, name: &str) -> Result<Vec<u8>, String> {
    let path = safe_path(dir, name)?;
    let encrypted = std::fs::read(path).map_err(|_| "图片附件不存在或无法读取")?;
    let bytes =
        storage::unprotect(&encrypted).map_err(|_| "图片附件损坏或不属于当前 Windows 账户")?;
    encode_image(&bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Upgrade old attachments in place, retaining their referenced filenames.
/// Atomic replacement means an interrupted migration is safe to repeat.
pub fn migrate_legacy(dir: &Path) -> Result<(), String> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("img_") {
            continue;
        }
        let path = safe_path(dir, &name)?;
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        // Old image files start with their real signature, never a DPAPI envelope.
        if crate::deepseek::sniff_image_mime(&bytes).is_some() {
            let encrypted = storage::protect(&bytes)?;
            if storage::unprotect(&encrypted)? != bytes {
                return Err("旧附件迁移校验失败，已保留原文件".into());
            }
            storage::atomic_write(&path, &encrypted)?;
        } else {
            storage::unprotect(&bytes)
                .map_err(|_| format!("附件 {name} 无法验证，已保留原文件"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_dir() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "taskvault_attachments_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn capture_accepts_raw_base64_and_data_urls_for_all_four_formats() {
        for bytes in [
            b"\x89PNG\r\n\x1a\nfixture".as_slice(),
            b"\xff\xd8\xfffixture",
            b"GIF89afixture",
            b"RIFF1234WEBPfixture",
        ] {
            let raw = B64.encode(bytes);
            assert_eq!(decode_capture(&raw).unwrap(), bytes);
            assert_eq!(
                decode_capture(&format!("data:image/png;base64,{raw}")).unwrap(),
                bytes
            );
        }
        assert!(decode_capture("not valid base64").is_err());
        assert!(decode_capture("data:text/plain;base64,aGVsbG8=").is_err());
        let large = [b"GIF89a".as_slice(), &vec![0; MAX_IMAGE_BYTES]].concat();
        assert!(decode_capture(&B64.encode(large)).is_err());
    }

    #[test]
    fn rejects_paths_outside_attachment_directory() {
        let dir = temp_dir();
        for path in [
            "../settings.json",
            "..\\settings.json",
            "C:\\x.png",
            "x.png:stream",
            "",
        ] {
            assert!(safe_path(&dir, path).is_err());
        }
    }

    #[test]
    fn saved_images_are_encrypted_and_legacy_migration_is_repeatable() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let bytes = b"GIF89a-private-image-fixture";
        let name = save(&dir, bytes).unwrap();
        assert_eq!(save(&dir, bytes).unwrap(), name);
        assert_ne!(std::fs::read(dir.join(&name)).unwrap(), bytes);
        assert_eq!(read(&dir, &name).unwrap(), bytes);
        let legacy = dir.join("img_20260916_120000.gif");
        std::fs::write(&legacy, bytes).unwrap();
        migrate_legacy(&dir).unwrap();
        migrate_legacy(&dir).unwrap();
        assert_eq!(read(&dir, "img_20260916_120000.gif").unwrap(), bytes);
        assert_ne!(std::fs::read(legacy).unwrap(), bytes);
        assert!(read(&dir, "missing.tvimg").is_err());
    }
}
