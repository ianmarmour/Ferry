//! ## Desktop Integration (Linux)
//!
//! Wayland compositors take an application's icon from its desktop entry, matched by
//! the window's application id, not from the window. `ferry --install` installs the
//! entry and the icons for the current user, so Ferry shows its icon in the window
//! decorations, the dock or taskbar, the application launcher and its notifications.
//!
//! Ferry installs them by itself when it starts, unless a package installed them for
//! every user: the RPM and the Debian package install the same entry, from
//! `packaging/linux`, launching `/usr/bin/ferry`. Run from an AppImage, the entry
//! launches the AppImage.
//!
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use crate::ui::icon;
use crate::ui::APP_ID;

fn data_home() -> io::Result<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or_else(|| io::Error::other("neither XDG_DATA_HOME nor HOME is set"))
}

fn entry_path(data: &Path) -> PathBuf {
    data.join("applications")
        .join(format!("{}.desktop", APP_ID))
}

fn icon_paths(data: &Path) -> Vec<(PathBuf, &'static [u8])> {
    let theme = data.join("icons/hicolor");
    let mut paths: Vec<(PathBuf, &'static [u8])> = icon::PNGS
        .iter()
        .map(|&(size, png)| {
            let path = theme.join(format!("{size}x{size}/apps/{APP_ID}.png"));
            (path, png)
        })
        .collect();
    paths.push((theme.join(format!("scalable/apps/{APP_ID}.svg")), icon::SVG));
    paths
}

/// The desktop entry the packages install, launching `ferry` from the `PATH`.
const PACKAGED_ENTRY: &str = include_str!("../packaging/linux/io.github.ianmarmour.Ferry.desktop");

/// The desktop entry, launching `executable`.
fn entry(executable: &Path) -> String {
    let exec = executable.display().to_string().replace(' ', "\\s");
    PACKAGED_ENTRY.replace("\nExec=ferry\n", &format!("\nExec={exec}\n"))
}

/// Whether `text` is an entry Ferry installed, launching any executable.
fn is_own_entry(text: &str) -> bool {
    let without_exec = |entry: &str| {
        entry
            .lines()
            .filter(|line| !line.starts_with("Exec="))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    without_exec(text) == without_exec(PACKAGED_ENTRY)
}

/// What the desktop entry launches: the AppImage Ferry runs from, if any, as the
/// AppImage unpacks the binary inside it somewhere new each time, or this binary.
fn launcher() -> io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let appimage = std::env::var_os("APPIMAGE");
    let appdir = std::env::var_os("APPDIR");
    Ok(launcher_of(
        executable,
        appimage.as_deref(),
        appdir.as_deref(),
    ))
}

fn launcher_of(executable: PathBuf, appimage: Option<&OsStr>, appdir: Option<&OsStr>) -> PathBuf {
    match (appimage, appdir) {
        (Some(appimage), Some(appdir)) if executable.starts_with(appdir) => PathBuf::from(appimage),
        _ => executable,
    }
}

/// The desktop entry a package installed for every user, if any.
fn system_entry(data: &Path) -> Option<PathBuf> {
    let dirs = std::env::var_os("XDG_DATA_DIRS");
    let appdir = std::env::var_os("APPDIR");
    system_entry_in(dirs.as_deref(), data, appdir.as_deref().map(Path::new))
}

/// The desktop entry in the data folders `dirs`, as `XDG_DATA_DIRS` lists them,
/// other than the user's own, `data`, and those of the AppImage in `appdir`.
fn system_entry_in(dirs: Option<&OsStr>, data: &Path, appdir: Option<&Path>) -> Option<PathBuf> {
    let dirs = dirs
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or(OsStr::new("/usr/local/share:/usr/share"));
    std::env::split_paths(dirs)
        .filter(|dir| dir.is_absolute() && dir != data)
        .filter(|dir| appdir.is_none_or(|appdir| !dir.starts_with(appdir)))
        .map(|dir| entry_path(&dir))
        .find(|path| path.is_file())
}

/// Refreshes the caches of the desktop, ignoring tools that are not installed.
fn refresh(data: &Path) {
    let commands: [(&str, Vec<PathBuf>); 2] = [
        (
            "gtk-update-icon-cache",
            vec![
                PathBuf::from("-f"),
                PathBuf::from("-t"),
                data.join("icons/hicolor"),
            ],
        ),
        ("update-desktop-database", vec![data.join("applications")]),
    ];
    for (command, arguments) in commands {
        let _ = std::process::Command::new(command)
            .args(arguments)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// Installs the desktop entry and icons for the current user.
pub fn install() -> io::Result<()> {
    let data = data_home()?;
    let executable = launcher()?;

    for (path, bytes) in icon_paths(&data) {
        std::fs::create_dir_all(path.parent().expect("icon paths have parents"))?;
        std::fs::write(&path, bytes)?;
    }
    let entry_path = entry_path(&data);
    std::fs::create_dir_all(entry_path.parent().expect("entry paths have parents"))?;
    std::fs::write(&entry_path, entry(&executable))?;
    refresh(&data);

    println!("Installed {}", entry_path.display());
    println!(
        "Ferry is now in your applications, launching {}.",
        executable.display()
    );
    println!("If you move it, run it with --install again.");
    Ok(())
}

/// The application id of Tidings, as Ferry was briefly called.
const LEGACY_APP_ID: &str = "io.github.ianmarmour.Tidings";

/// Removes the desktop entry and icons Tidings installed.
pub fn remove_legacy() {
    let Ok(data) = data_home() else { return };
    let theme = data.join("icons/hicolor");
    let mut paths = vec![data
        .join("applications")
        .join(format!("{LEGACY_APP_ID}.desktop"))];
    paths.extend(
        icon::PNGS
            .iter()
            .map(|(size, _)| theme.join(format!("{size}x{size}/apps/{LEGACY_APP_ID}.png"))),
    );
    paths.push(theme.join(format!("scalable/apps/{LEGACY_APP_ID}.svg")));

    let removed = paths
        .iter()
        .filter(|path| std::fs::remove_file(path).is_ok())
        .count();
    if removed > 0 {
        refresh(&data);
        log::info!("removed the desktop entry of Tidings, a name Ferry briefly had");
    }
}

/// Removes what [`install`] installed.
pub fn uninstall() -> io::Result<()> {
    let data = data_home()?;
    for path in remove(&data)? {
        println!("Removed {}", path.display());
    }
    if let Some(entry) = system_entry(&data) {
        println!(
            "{} remains, installed by a package for every user.",
            entry.display()
        );
    }
    Ok(())
}

/// Removes the desktop entry and icons from the data folder `data`, and returns the
/// files it removed.
fn remove(data: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = icon_paths(data).into_iter().map(|(path, _)| path).collect();
    paths.push(entry_path(data));
    let mut removed = Vec::new();
    for path in paths {
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    refresh(data);
    Ok(removed)
}

/// Installs the desktop entry and icons unless they are installed for this binary,
/// for this user or, by a package, for every user.
pub fn ensure_installed() {
    let Ok(data) = data_home() else { return };
    if let Some(system) = system_entry(&data) {
        // The entry Ferry installed for this user before, say from the tarball, would
        // hide the package's, and launch the old binary.
        let own = std::fs::read_to_string(entry_path(&data));
        if own.is_ok_and(|text| is_own_entry(&text)) {
            match remove(&data) {
                Ok(_) => log::info!(
                    "removed the desktop entry installed for this user, for {}",
                    system.display()
                ),
                Err(error) => log::warn!("cannot remove the desktop entry: {}", error),
            }
        }
        return;
    }
    let Ok(executable) = launcher() else { return };
    let current = std::fs::read_to_string(entry_path(&data)).ok();
    if current.as_deref() == Some(entry(&executable).as_str()) {
        return;
    }
    let quiet = || -> io::Result<()> {
        for (path, bytes) in icon_paths(&data) {
            std::fs::create_dir_all(path.parent().expect("icon paths have parents"))?;
            std::fs::write(&path, bytes)?;
        }
        let entry_path = entry_path(&data);
        std::fs::create_dir_all(entry_path.parent().expect("entry paths have parents"))?;
        std::fs::write(&entry_path, entry(&executable))
    };
    match quiet() {
        Ok(()) => {
            refresh(&data);
            log::info!("installed the desktop entry and icons");
        }
        Err(error) => log::warn!("cannot install the desktop entry: {}", error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_names_the_icon_and_window_class() {
        let entry = entry(Path::new("/opt/My Apps/ferry"));
        assert!(entry.contains("\nExec=/opt/My\\sApps/ferry\n"));
        assert!(!entry.contains("Exec=ferry"));
        assert!(entry.contains(&format!("Icon={}\n", APP_ID)));
        assert!(entry.contains(&format!("StartupWMClass={}\n", APP_ID)));
        assert!(is_own_entry(&entry));
        assert!(!is_own_entry(&entry.replace("Name=Ferry", "Name=Other")));
    }

    #[test]
    fn appimages_launch_the_appimage() {
        let binary = PathBuf::from("/tmp/.mount_FerryX/usr/bin/ferry");
        let image = Some(OsStr::new("/home/me/Ferry.AppImage"));
        let appdir = Some(OsStr::new("/tmp/.mount_FerryX"));
        assert_eq!(
            PathBuf::from("/home/me/Ferry.AppImage"),
            launcher_of(binary.clone(), image, appdir)
        );
        assert_eq!(binary, launcher_of(binary.clone(), None, None));
        // An AppImage that started Ferry from elsewhere doesn't count.
        let other = PathBuf::from("/usr/bin/ferry");
        assert_eq!(other, launcher_of(other.clone(), image, appdir));
    }

    #[test]
    fn finds_entries_packages_installed() {
        let root = std::env::temp_dir().join(format!("ferry-desktop-{}", std::process::id()));
        let user = root.join("home/.local/share");
        let system = root.join("usr/share");
        let appimage = root.join("mount/usr/share");
        for data in [&user, &system, &appimage] {
            let path = entry_path(data);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, PACKAGED_ENTRY).unwrap();
        }
        let dirs = |list: &[&Path]| std::env::join_paths(list).unwrap();

        let all = dirs(&[&appimage, &user, &root.join("none"), &system]);
        let found = system_entry_in(Some(&all), &user, Some(&root.join("mount")));
        assert_eq!(Some(entry_path(&system)), found);
        let theirs = dirs(&[&appimage, &user]);
        assert_eq!(
            None,
            system_entry_in(Some(&theirs), &user, Some(&root.join("mount")))
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn icons_go_in_the_hicolor_theme() {
        let paths = icon_paths(Path::new("/data"));
        assert!(paths.iter().any(|(path, _)| path
            == Path::new(&format!("/data/icons/hicolor/256x256/apps/{}.png", APP_ID))));
        assert!(paths.iter().any(|(path, _)| path
            == Path::new(&format!("/data/icons/hicolor/scalable/apps/{}.svg", APP_ID))));
    }
}
