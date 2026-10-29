fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    println!("cargo:rerun-if-changed=../../assets/icons/omd.ico");

    // Check the target rather than the build host so cross builds embed the icon too.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // Version fields inherit the workspace package version through Cargo.
    winresource::WindowsResource::new()
        .set_icon("../../assets/icons/omd.ico")
        .set("ProductName", "ohMyDisk")
        .set("FileDescription", "ohMyDisk disk space analyzer")
        .set("InternalName", "ohmydisk")
        .set("OriginalFilename", "ohmydisk.exe")
        .compile()
        .expect("failed to compile the ohMyDisk Windows icon and version resources");
}
