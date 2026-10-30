//! Ferry: text from your computer through your iPhone, over Bluetooth.
//!
//! ```text
//! ferry          connect to a paired iPhone
//! ferry --demo   try Ferry with a simulated iPhone
//! ```
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::sync::Arc;
use std::time::Duration;

use ferry::bluetooth::{self, demo::DemoPhone, Backend};

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("ferry=info"))
        .init();

    let demo = std::env::args().skip(1).any(|arg| arg == "--demo");
    if std::env::args()
        .skip(1)
        .any(|arg| arg == "--help" || arg == "-h")
    {
        println!("Usage: ferry [--demo]\n\n  --demo   try Ferry with a simulated iPhone");
        return Ok(());
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("ferry")
        .build()?;

    let backend: Arc<dyn Backend> = if demo {
        let phone = {
            let _runtime = runtime.enter();
            let phone = DemoPhone::new();
            phone.chat_every(Duration::from_secs(90));
            phone
        };
        Arc::new(phone)
    } else {
        bluetooth::native()?
    };

    ferry::ui::run(backend, runtime.handle().clone(), demo)
        .map_err(|error| anyhow::anyhow!("{}", error))?;
    runtime.shutdown_timeout(Duration::from_secs(2));
    Ok(())
}
