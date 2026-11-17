//! Gives gridwatch.exe its icon and version details on Windows.

fn main() {
    println!("cargo:rerun-if-changed=packaging/icon.ico");
    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("packaging/icon.ico");
        resource.set("ProductName", "gridwatch");
        resource.set("FileDescription", "gridwatch");
        resource.set("LegalCopyright", "voidNK");
        if let Err(e) = resource.compile() {
            // Without the Windows SDK the program still builds, only without its icon.
            println!("cargo:warning=no icon embedded: {e}");
        }
    }
}
