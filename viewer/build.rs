//! On Windows: gives k-pdf.exe its icon and version information (shown in
//! Explorer and the task bar).

fn main() {
    println!("cargo:rerun-if-changed=../packaging/windows/k-pdf.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../packaging/windows/k-pdf.ico")
            .set("ProductName", "k-pdf")
            .set("FileDescription", "k-pdf PDF viewer and editor")
            .set("LegalCopyright", "Copyright (c) 2026 Kabilan Mahathevan (MIT OR Apache-2.0)");
        res.compile().expect("could not add the icon to k-pdf.exe");
    }
}
