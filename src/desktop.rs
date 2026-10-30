//! ## Desktop Integration (Linux)
//!
//! Wayland compositors take an application's icon from its desktop entry, matched by
//! the window's application id, not from the window. `ferry --install` installs the
//! entry and the icons for the current user, so Ferry shows its icon in the window
//! decorations, the dock or taskbar, the application launcher and its notifications.
//!
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

/// The desktop entry, launching `executable`.
fn entry(executable: &Path) -> String {
    let exec = executable.display().to_string().replace(' ', "\\s");
    format!(
        "[Desktop Entry]
Type=Application
Name=Ferry
GenericName=Messages
Comment=Text from your computer through your iPhone
Exec={exec}
Icon={APP_ID}
Terminal=false
Categories=Network;InstantMessaging;Chat;
Keywords=iPhone;SMS;iMessage;Messages;Bluetooth;Text;
StartupWMClass={APP_ID}
StartupNotify=true
"
    )
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
    let executable = std::env::current_exe()?;

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
    println!("If you move the binary, run `ferry --install` again.");
    Ok(())
}

/// Removes what [`install`] installed.
pub fn uninstall() -> io::Result<()> {
    let data = data_home()?;
    let mut paths: Vec<PathBuf> = icon_paths(&data)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    paths.push(entry_path(&data));
    for path in paths {
        match std::fs::remove_file(&path) {
            Ok(()) => println!("Removed {}", path.display()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    refresh(&data);
    Ok(())
}

/// Installs the desktop entry and icons unless they are installed for this binary.
pub fn ensure_installed() {
    let (Ok(data), Ok(executable)) = (data_home(), std::env::current_exe()) else {
        return;
    };
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
        assert!(entry.contains("Exec=/opt/My\\sApps/ferry\n"));
        assert!(entry.contains(&format!("Icon={}\n", APP_ID)));
        assert!(entry.contains(&format!("StartupWMClass={}\n", APP_ID)));
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
