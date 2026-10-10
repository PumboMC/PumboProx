//! On Windows, `pumboprox.exe` gets the Pumbo icon and the file details
//! Explorer shows (product name, version). Other systems keep no icon in the
//! binary.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=pumboprox.ico");
    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            windows_resources();
        }
    }
}

/// Writes the resource script into the build output, with the icon by its
/// full path and the version of the crate, and links it into the binary.
#[cfg(windows)]
fn windows_resources() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let n: Vec<u16> = version
        .split(['.', '-'])
        .take(3)
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    let (a, b, c) = (n[0], n[1], n[2]);
    let icon = std::path::Path::new(&dir)
        .join("pumboprox.ico")
        .display()
        .to_string()
        .replace('\\', "\\\\");
    let script = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "PumboMC"
      VALUE "FileDescription", "PumboProx, a proxy for Pumpkin servers"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "pumboprox"
      VALUE "LegalCopyright", "GPL-3.0"
      VALUE "OriginalFilename", "pumboprox.exe"
      VALUE "ProductName", "PumboProx"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let rc = out.join("pumboprox.rc");
    std::fs::write(&rc, script).expect("write pumboprox.rc");
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_optional()
        .expect("compile pumboprox.rc");
}
