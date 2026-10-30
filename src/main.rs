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

const USAGE: &str = "Usage: ferry [--demo]

  --demo        try Ferry with a simulated iPhone
  --install     add Ferry and its icon to your applications (Linux)
  --uninstall   remove them again (Linux)";

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("ferry=info"))
        .init();

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| arguments.iter().any(|argument| argument == name);
    if flag("--help") || flag("-h") {
        println!("{}", USAGE);
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        if flag("--install") {
            return Ok(ferry::desktop::install()?);
        }
        if flag("--uninstall") {
            return Ok(ferry::desktop::uninstall()?);
        }
    }
    let demo = flag("--demo");
    #[cfg(target_os = "linux")]
    if !demo {
        ferry::desktop::ensure_installed();
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
