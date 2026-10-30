//! ## Migration from Ferry
//!
//! Tidings used to be called Ferry. On first launch, this moves Ferry's message
//! caches, contact photos, attachments and settings to the folders of Tidings, points
//! the paths saved in the caches at their new place, and removes Ferry's desktop entry.
//!
use std::path::Path;

/// The former name, which named the folders.
const LEGACY_NAME: &str = "Ferry";

/// Moves what Ferry left behind, if anything, and if Tidings has nothing yet.
pub fn from_ferry() {
    let legacy = directories::ProjectDirs::from("", "", LEGACY_NAME);
    let current = directories::ProjectDirs::from("", "", "Tidings");
    if let (Some(legacy), Some(current)) = (legacy, current) {
        move_folder(legacy.data_dir(), current.data_dir());
        move_folder(legacy.config_dir(), current.config_dir());
    }

    #[cfg(target_os = "linux")]
    crate::desktop::remove_legacy();
}

/// Moves `old` to `new` unless `new` exists, and fixes the paths saved inside.
fn move_folder(old: &Path, new: &Path) {
    if !old.exists() || new.exists() {
        return;
    }
    if let Some(parent) = new.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::rename(old, new) {
        Ok(()) => {
            log::info!("moved {} to {}", old.display(), new.display());
            rewrite_paths(new, old, new);
            // Windows keeps data and settings in subfolders of a folder named Ferry.
            if let Some(parent) = old.parent() {
                if parent.file_name().is_some_and(|name| name == LEGACY_NAME) {
                    let _ = std::fs::remove_dir(parent);
                }
            }
        }
        Err(error) => log::warn!("cannot move {}: {}", old.display(), error),
    }
}

/// Replaces `old` with `new` in the paths saved in the JSON files of `folder` and
/// its `phones` folder.
fn rewrite_paths(folder: &Path, old: &Path, new: &Path) {
    let escape = |path: &Path| {
        serde_json::to_string(&path.to_string_lossy())
            .map(|text| text.trim_matches('"').to_string())
    };
    let (Ok(old), Ok(new)) = (escape(old), escape(new)) else {
        return;
    };

    for directory in [folder.to_path_buf(), folder.join("phones")] {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if text.contains(&old) {
                if let Err(error) = std::fs::write(&path, text.replace(&old, &new)) {
                    log::warn!("cannot update {}: {}", path.display(), error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_folders_and_their_saved_paths() {
        let root = std::env::temp_dir().join(format!("tidings-migrate-{}", std::process::id()));
        let old = root.join("ferry");
        let new = root.join("tidings");
        std::fs::create_dir_all(old.join("phones/photos")).unwrap();
        std::fs::write(old.join("phones/photos/a.png"), b"png").unwrap();
        let saved = old.join("phones/photos/a.png");
        let json = serde_json::json!({ "photo": saved.to_string_lossy() }).to_string();
        std::fs::write(old.join("phones/AA-BB.json"), json).unwrap();

        move_folder(&old, &new);

        assert!(!old.exists());
        let moved = std::fs::read_to_string(new.join("phones/AA-BB.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&moved).unwrap();
        let photo = std::path::PathBuf::from(value["photo"].as_str().unwrap());
        assert_eq!(new.join("phones/photos/a.png"), photo);
        assert_eq!(b"png".to_vec(), std::fs::read(photo).unwrap());

        // Nothing happens once Tidings has its own folder.
        std::fs::create_dir_all(&old).unwrap();
        move_folder(&old, &new);
        assert!(old.exists());

        std::fs::remove_dir_all(root).unwrap();
    }
}
