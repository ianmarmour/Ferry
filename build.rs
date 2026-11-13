//! Embeds the application icon in Windows executables.

fn main() {
    println!("cargo:rerun-if-changed=assets/ferry.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/ferry.ico");
        if let Err(error) = resource.compile() {
            println!("cargo:warning=cannot embed the application icon: {}", error);
        }
    }
}
