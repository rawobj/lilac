//! Windows: the exe's icon and the details Explorer shows under Properties
//! (name, version, publisher). gpui already embeds the application manifest.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=packaging/windows/lilac.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    windows::embed_resources();
}

mod windows {
    use std::env;
    use std::path::PathBuf;

    pub fn embed_resources() {
        let version = env::var("CARGO_PKG_VERSION").unwrap();
        let mut parts = version
            .split(['.', '-', '+'])
            .map(|p| p.parse::<u16>().unwrap_or(0));
        let [major, minor, patch] = [(); 3].map(|_| parts.next().unwrap_or(0));
        let description = env::var("CARGO_PKG_DESCRIPTION").unwrap();
        let icon = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("packaging/windows/lilac.ico");

        // ProductName and ProductVersion stay the same for every file of a
        // release; code signing checks them against the signing policy.
        let rc = format!(
            r#"#pragma code_page(65001)
1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Robin"
      VALUE "FileDescription", "Lilac"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "lilac"
      VALUE "LegalCopyright", "Copyright (c) 2026 Robin. MIT License."
      VALUE "OriginalFilename", "lilac.exe"
      VALUE "ProductName", "Lilac"
      VALUE "ProductVersion", "{version}"
      VALUE "Comments", "{description}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
            icon = icon.display().to_string().replace('\\', "\\\\"),
        );
        let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("lilac.rc");
        std::fs::write(&path, rc).unwrap();
        embed_resource::compile(&path, embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
