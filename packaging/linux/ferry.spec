# Ferry, with its desktop entry, icons and AppStream metadata, so that it shows
# in GNOME's, KDE Plasma's and other desktops' menus and software centers.
# build-rpm.sh builds it from a built ferry, staged by stage.sh, defining
# ferry_source, ferry_version and ferry_release.

# The program is built already, by cargo.
%global debug_package %{nil}
%global __os_install_post %{nil}
%global _build_id_links none
%global app_id io.github.ianmarmour.Ferry

Name:           ferry
Version:        %{ferry_version}
Release:        %{ferry_release}%{?dist}
Summary:        Text from your computer through your iPhone
License:        MIT
URL:            https://github.com/ianmarmour/Ferry
Packager:       Ian Armour <ianmarmour@gmail.com>
Vendor:         Ian Armour
BuildArch:      x86_64
# The windowing library loads these as it starts, rather than linking them,
# so rpmbuild doesn't find them. Keyboards need xkbcommon on X11 and Wayland.
Requires:       libxkbcommon.so.0()(64bit)
Recommends:     libxkbcommon-x11.so.0()(64bit)
Recommends:     libwayland-client.so.0()(64bit)
Recommends:     libwayland-egl.so.1()(64bit)
Recommends:     libEGL.so.1()(64bit)
Recommends:     libGL.so.1()(64bit)
# Ferry finds the paired iPhone and publishes its notification service
# through BlueZ.
Recommends:     bluez

%description
Ferry brings your iPhone's messages to your desktop over Bluetooth. Read your
conversations, reply, and start new ones with anyone in your contacts, in a
window styled after Messages on the Mac. It talks to the phone the way a car
does, so there's no app to install on the iPhone, no Mac to relay through and
no cloud service in between.

%install
mkdir -p %{buildroot}%{_prefix}
cp -r %{ferry_source}/. %{buildroot}%{_prefix}/

%files
%{_bindir}/ferry
%{_datadir}/applications/%{app_id}.desktop
%{_datadir}/metainfo/%{app_id}.metainfo.xml
%{_datadir}/icons/hicolor/*/apps/%{app_id}.*
%license %{_datadir}/licenses/ferry/LICENSE
%doc %{_docdir}/ferry/README.md
