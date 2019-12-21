use crate::units::*;
use std::fs::read_to_string;
use std::path::PathBuf;

pub fn parse_target(path: &PathBuf, chosen_id: InternalId) -> Result<Unit, String> {
    let raw = read_to_string(&path)
        .map_err(|e| format!("Error opening file: {:?} error: {}", path, e))?;
    let parsed_file = parse_file(&raw);

    let mut install_config = None;
    let mut unit_config = None;

    for (name, section) in parsed_file {
        match name.as_str() {
            "[Unit]" => {
                unit_config = Some(parse_unit_section(section, path));
            }
            "[Install]" => {
                install_config = Some(parse_install_section(section));
            }
            _ => panic!("Unknown section name: {}", name),
        }
    }

    let conf = match unit_config {
        Some(conf) => conf,
        None => return Err(format!("Didn't find a unit config for file: {:?}", path)),
    };

    Ok(Unit {
        conf,
        id: chosen_id,
        install: Install {
            install_config: install_config,
            wants: Vec::new(),
            wanted_by: Vec::new(),
            requires: Vec::new(),
            required_by: Vec::new(),
            before: Vec::new(),
            after: Vec::new(),
        },
        specialized: UnitSpecialized::Target,
    })
}
