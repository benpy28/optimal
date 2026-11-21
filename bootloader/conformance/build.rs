fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let pie = std::env::var("OB_PIE").is_ok_and(|v| v == "1");
    let script = if pie { "link-pie.ld" } else { "link.ld" };
    println!("cargo:rustc-link-arg=-T{dir}/{script}");
    if pie {
        println!("cargo:rustc-link-arg=-pie");
        println!("cargo:rustc-link-arg=-zpack-relative-relocs");
    }
    for v in [
        "OB_PIE",
        "OB_REVISION",
        "OB_PAGING",
        "OB_ENTRY_REQ",
        "OB_DUPLICATE",
        "OB_EXPECT_CMDLINE",
        "OB_EXPECT_MODULES",
    ] {
        println!("cargo:rerun-if-env-changed={v}");
    }
    println!("cargo:rerun-if-changed={dir}/link.ld");
    println!("cargo:rerun-if-changed={dir}/link-pie.ld");
}
