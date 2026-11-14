use photo_sorter::{
    config::Config,
    qualification::{
        QualificationRequest, persist_report, qualify_generated_outputs, report_qualified,
    },
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let profile = std::env::args_os()
        .nth(1)
        .ok_or("usage: qualify_generated_outputs PROFILE.toml")?;
    let config = Config::load(std::path::Path::new(&profile))?;
    let report = qualify_generated_outputs(&config)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report_qualified(&QualificationRequest::GeneratedOutputs, &report) {
        return Err("generated replacement scratch qualification failed".into());
    }
    let path = persist_report(&config, &report)?;
    eprintln!(
        "Qualified generated replacement; evidence saved at {}",
        path.display()
    );
    Ok(())
}
