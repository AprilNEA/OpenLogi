fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/flow.v1.proto");
    buffa_build::Config::new()
        .files(&["proto/flow.v1.proto"])
        .includes(&["proto"])
        .include_file("_include.rs")
        .compile()?;
    Ok(())
}
