//! Wrap the example guest's generated WIT metadata in a standard component.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let input = args
        .next()
        .ok_or("usage: componentize INPUT.wasm OUTPUT.wasm")?;
    let output = args
        .next()
        .ok_or("usage: componentize INPUT.wasm OUTPUT.wasm")?;
    let bytes = std::fs::read(input)?;
    let component = wit_component::ComponentEncoder::default()
        .module(&bytes)?
        .validate(true)
        .encode()?;
    std::fs::write(output, component)?;
    Ok(())
}
