//! Windows version info for deafoverlay.exe, shown in its Properties > Details. File and product
//! versions come from this crate's `version`, so a bump in Cargo.toml is all a release needs.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "Heroes Without Sound")
        .set("FileDescription", "Heroes Without Sound")
        .set("OriginalFilename", "deafoverlay.exe");
    res.compile().expect("compiling the version resource (needs the Windows SDK's rc.exe)");
}
