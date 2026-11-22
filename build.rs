//! The Windows build gets Conductor's icon and name in its file properties. Without a resource compiler (windres /
//! rc.exe) the program is still built, just without the icon.
fn main() {
    println!("cargo:rerun-if-changed=assets/brand/conductor.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/brand/conductor.ico");
    res.set("ProductName", "Conductor");
    res.set("FileDescription", "Conductor music server");
    if let Err(e) = res.compile() {
        println!("cargo:warning=building without the icon: {e}");
    }
}
