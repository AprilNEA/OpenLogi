fn main() {
    println!("cargo:rerun-if-env-changed=OPENLOGI_UPDATE_MANIFEST_URL");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=resources/windows/openlogi.rc");
        println!("cargo:rerun-if-changed=resources/windows/openlogi.ico");
        if let Err(error) =
            embed_resource::compile("resources/windows/openlogi.rc", embed_resource::NONE)
                .manifest_optional()
        {
            println!("cargo:warning=failed to compile Windows application resources: {error}");
            std::process::exit(1);
        }
    }
}
