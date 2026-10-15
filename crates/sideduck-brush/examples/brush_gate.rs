fn main() -> Result<(), String> {
    let report = sideduck_brush::run_throughput_gate()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );
    Ok(())
}
