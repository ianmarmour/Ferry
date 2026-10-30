//! # Tidings
//!
//! Texting from the desktop through an iPhone, over Bluetooth, with nothing to install
//! on the phone. Tidings is a Message Access Profile client (MAP MCE), like a car kit:
//! it reads and sends messages through the phone's Message Access Server, is notified
//! of new messages by the phone, and downloads contacts over the Phone Book Access
//! Profile.
//!
//! * [`bluetooth`]: the platform Bluetooth stacks, and a simulated phone.
//! * [`session`]: the worker that talks to the phone.
//! * [`model`], [`contacts`], [`text`]: messages, conversations and contacts.
//! * [`pbap`]: a minimal Phone Book Access client.
//! * [`ui`]: the window.
//!
pub mod bluetooth;
pub mod contacts;
#[cfg(target_os = "linux")]
pub mod desktop;
pub mod migrate;
pub mod model;
pub mod notify;
pub mod pbap;
pub mod session;
pub mod text;
pub mod ui;
