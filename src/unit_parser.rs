use crate::units::*;

use crate::services::{Service, ServiceStatus};
use crate::sockets::{
    Socket, SocketKind, SpecializedSocketConfig, TcpSocketConfig, UdpSocketConfig, UnixSocketConfig,
};

use std::collections::HashMap;
use std::fs::read_to_string;
use std::path::PathBuf;

type ParsedSection = HashMap<String, Vec<(u32, String)>>;
type ParsedFile = HashMap<String, ParsedSection>;

fn parse_section(lines: &Vec<&str>) -> ParsedSection {
    let mut entries: ParsedSection = HashMap::new();

    let mut entry_number = 0;
    for line in lines {
        let pos = if let Some(pos) = line.find(|c| c == '=') {
            pos
        } else {
            continue;
        };
        let (name, value) = line.split_at(pos);

        let value = value.trim_start_matches("=");
        let value = value.trim();
        let name = name.trim().to_uppercase();
        let values: Vec<String> = value.split(",").map(|x| x.to_owned()).collect();

        let vec = match entries.get_mut(&name) {
            Some(vec) => vec,
            None => {
                entries.insert(name.clone(), Vec::new());
                entries.get_mut(&name).unwrap()
            }
        };

        for value in values {
            vec.push((entry_number, value));
            entry_number += 1;
        }
    }

    entries
}

fn parse_file(content: &String) -> ParsedFile {
    let mut sections = HashMap::new();
    let lines: Vec<&str> = content.split("\n").collect();

    let mut lines_left = &lines[..];

    let mut current_section_name = "".to_string();
    let mut current_section_lines = Vec::new();
    while lines_left.len() > 0 {
        for idx in 0..lines_left.len() {
            let line = lines_left[idx];
            if current_section_name == "" {
                current_section_name = line.into();
                current_section_lines.clear();
            } else {
                if line.starts_with("[") || idx == lines_left.len() - 1 {
                    sections.insert(
                        current_section_name.clone(),
                        parse_section(&current_section_lines),
                    );
                    current_section_name = line.into();
                    current_section_lines.clear();
                    lines_left = &lines_left[idx + 1..];
                    break;
                } else {
                    current_section_lines.push(line.into());
                }
            }
        }
    }

    sections
}

fn parse_socket(path: &PathBuf, chosen_id: InternalId) -> Result<Unit, String> {
    let raw = read_to_string(&path).unwrap();
    let parsed_file = parse_file(&raw);

    let mut socket_configs = None;
    let mut install_config = None;
    let mut unit_config = None;

    for (name, section) in parsed_file {
        match name.as_str() {
            "[Socket]" => {
                socket_configs = match parse_socket_section(section) {
                    Ok(conf) => Some(conf),
                    Err(e) => return Err(format!("Error in file: {:?} :: {}", path, e)),
                };
            }
            "[Unit]" => {
                unit_config = Some(parse_unit_section(section, path));
            }
            "[Install]" => {
                install_config = Some(parse_install_section(section));
            }

            _ => panic!("Unknown section name: {}", name),
        }
    }

    // TODO handle install configs for sockets
    let _ = install_config;

    let (sock_name, services, sock_configs) = socket_configs.unwrap(); 

    Ok(Unit {
        conf: unit_config.unwrap().clone(),
        id: chosen_id,
        install: Install::default(),
        specialized: UnitSpecialized::Socket(Socket {
            name: sock_name,
            sockets: sock_configs,
            services: services,
        }),
    })
}

fn parse_service(path: &PathBuf, chosen_id: InternalId) -> Unit {
    let raw = read_to_string(&path).unwrap();
    let parsed_file = parse_file(&raw);

    let mut service_config = None;
    let mut install_config = None;
    let mut unit_config = None;

    for (name, section) in parsed_file {
        match name.as_str() {
            "[Service]" => {
                service_config = Some(parse_service_section(section));
            }
            "[Unit]" => {
                unit_config = Some(parse_unit_section(section, path));
            }
            "[Install]" => {
                install_config = Some(parse_install_section(section));
            }

            _ => panic!("Unknown section name: {}", name),
        }
    }

    Unit {
        id: chosen_id,
        conf: unit_config.unwrap_or(UnitConfig {
            filepath: path.clone(),

            wants: Vec::new(),
            requires: Vec::new(),
            before: Vec::new(),
            after: Vec::new(),
        }),
        install: Install {
            wants: Vec::new(),
            wanted_by: Vec::new(),
            requires: Vec::new(),
            required_by: Vec::new(),
            before: Vec::new(),
            after: Vec::new(),
            install_config: install_config,
        },
        specialized: UnitSpecialized::Service(Service {
            pid: None,
            status: ServiceStatus::NeverRan,

            service_config: service_config,

            sockets: Vec::new(),
        }),
    }
}

fn parse_unix_addr(addr: &str) -> Result<String, ()> {
    if addr.starts_with("/") || addr.starts_with("./") {
        Ok(addr.to_owned())
    } else {
        Err(())
    }
}

fn parse_ipv4_addr(addr: &str) -> Result<std::net::SocketAddrV4, std::net::AddrParseError> {
    let sock: Result<std::net::SocketAddrV4, std::net::AddrParseError> = addr.parse();
    sock
}

fn parse_ipv6_addr(addr: &str) -> Result<std::net::SocketAddrV6, std::net::AddrParseError> {
    let sock: Result<std::net::SocketAddrV6, std::net::AddrParseError> = addr.parse();
    sock
}

fn parse_socket_section(section: ParsedSection) -> Result<(String, Vec<String>, Vec<SocketConfig>), String> {
    let mut fdname: Option<String> = None;
    let mut socket_kinds: Vec<(u32, SocketKind)> = Vec::new();
    let mut services: Vec<String> = Vec::new();

    // TODO check that there is indeed exactly one value per name
    for (name, mut values) in section {
        match name.as_str() {
            "FILEDESCRIPTORNAME" => {
                fdname = Some(values.remove(0).1);
            }
            "LISTENSTREAM" => {
                for _ in 0..values.len() {
                    let (entry_num, value) = values.remove(0);
                    socket_kinds.push((entry_num, SocketKind::Stream(value)));
                }
            }
            "LISTENDATAGRAM" => {
                for _ in 0..values.len() {
                    let (entry_num, value) = values.remove(0);
                    socket_kinds.push((entry_num, SocketKind::Datagram(value)));
                }
            }
            "LISTENSEQUENTIALPACKET" => {
                for _ in 0..values.len() {
                    let (entry_num, value) = values.remove(0);
                    socket_kinds.push((entry_num, SocketKind::Sequential(value)));
                }
            }
            "SERVICE" => {
                for _ in 0..values.len() {
                    let (_, value) = values.remove(0);
                    services.push(value);
                }
            }
            _ => panic!("Unknown parameter name: {}", name),
        }
    }

    // we need to preserve the original ordering
    socket_kinds.sort_by(|l, r| u32::cmp(&l.0, &r.0));
    let socket_kinds: Vec<SocketKind> = socket_kinds.iter().map(|(_, kind)| kind.clone()).collect();

    let mut socket_configs = Vec::new();

    for kind in socket_kinds {
        let specialized: SpecializedSocketConfig = match &kind {
            SocketKind::Sequential(addr) => {
                if let Ok(_) = parse_unix_addr(addr) {
                    SpecializedSocketConfig::UnixSocket(UnixSocketConfig { kind: kind.clone() })
                } else {
                    return Err(format!(
                        "No specialized config for socket found for socket addr: {}",
                        addr
                    )
                    .into());
                }
            }
            SocketKind::Stream(addr) => {
                if let Ok(_) = parse_unix_addr(addr) {
                    SpecializedSocketConfig::UnixSocket(UnixSocketConfig { kind: kind.clone() })
                } else {
                    if let Ok(addr) = parse_ipv4_addr(addr) {
                        SpecializedSocketConfig::TcpSocket(TcpSocketConfig {
                            addr: std::net::SocketAddr::V4(addr),
                        })
                    } else {
                        if let Ok(addr) = parse_ipv6_addr(addr) {
                            SpecializedSocketConfig::TcpSocket(TcpSocketConfig {
                                addr: std::net::SocketAddr::V6(addr),
                            })
                        } else {
                            return Err(format!(
                                "No specialized config for socket found for socket addr: {}",
                                addr
                            )
                            .into());
                        }
                    }
                }
            }
            SocketKind::Datagram(addr) => {
                if let Ok(_) = parse_unix_addr(addr) {
                    SpecializedSocketConfig::UnixSocket(UnixSocketConfig { kind: kind.clone() })
                } else {
                    if let Ok(addr) = parse_ipv4_addr(addr) {
                        SpecializedSocketConfig::UdpSocket(UdpSocketConfig {
                            addr: std::net::SocketAddr::V4(addr),
                        })
                    } else {
                        if let Ok(addr) = parse_ipv6_addr(addr) {
                            SpecializedSocketConfig::UdpSocket(UdpSocketConfig {
                                addr: std::net::SocketAddr::V6(addr),
                            })
                        } else {
                            return Err(format!(
                                "No specialized config for socket found for socket addr: {}",
                                addr
                            )
                            .into());
                        }
                    }
                }
            }
        };

        socket_configs.push(SocketConfig {
            kind: kind,
            specialized: specialized,
            fd: None,
        });
    }

    let name = match fdname {
        Some(name) => name,
        None => "unknown".into(),
    };

    return Ok((name, services, socket_configs));
}

fn map_tupels_to_second<X, Y: Clone>(v: Vec<(X, Y)>) -> Vec<Y> {
    v.iter().map(|(_, scnd)| scnd.clone()).collect()
}

fn parse_unit_section(mut section: ParsedSection, path: &PathBuf) -> UnitConfig {
    let wants = section.remove("WANTS");
    let requires = section.remove("REQUIRES");
    let after = section.remove("AFTER");
    let before = section.remove("BEFORE");

    UnitConfig {
        filepath: path.clone(),
        wants: map_tupels_to_second(wants.unwrap_or(Vec::new())),
        requires: map_tupels_to_second(requires.unwrap_or(Vec::new())),
        after: map_tupels_to_second(after.unwrap_or(Vec::new())),
        before: map_tupels_to_second(before.unwrap_or(Vec::new())),
    }
}

fn parse_install_section(mut section: ParsedSection) -> InstallConfig {
    let wantedby = section.remove("WANTEDBY");
    let requiredby = section.remove("REQUIREDBY");

    InstallConfig {
        wanted_by: map_tupels_to_second(wantedby.unwrap_or(Vec::new())),
        required_by: map_tupels_to_second(requiredby.unwrap_or(Vec::new())),
    }
}

fn parse_service_section(mut section: ParsedSection) -> ServiceConfig {
    let exec = section.remove("EXEC");
    let stop = section.remove("STOP");
    let keep_alive = section.remove("KEEP_ALIVE");

    let exec = match exec {
        Some(mut vec) => {
            if vec.len() == 1 {
                vec.remove(0).1
            } else {
                panic!("Exec had to many entries: {:?}", vec);
            }
        }
        None => "".to_string(),
    };

    let stop = match stop {
        Some(mut vec) => {
            if vec.len() == 1 {
                vec.remove(0).1
            } else {
                panic!("Stop had to many entries: {:?}", vec);
            }
        }
        None => "".to_string(),
    };

    let keep_alive = match keep_alive {
        Some(vec) => {
            if vec.len() == 1 {
                vec[0].1 == "true"
            } else {
                panic!("Keepalive had to many entries: {:?}", vec);
            }
        }
        None => false,
    };

    ServiceConfig {
        keep_alive: keep_alive,
        exec: exec,
        stop: stop,
    }
}

pub fn parse_all_services(
    services: &mut std::collections::HashMap<InternalId, Unit>,
    path: &PathBuf,
    last_id: &mut InternalId,
) {
    let mut files: Vec<_> = std::fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    files.sort_by(|l, r| l.path().cmp(&r.path()));
    for entry in files {
        if entry.path().is_dir() {
            parse_all_services(services, path, last_id);
        } else {
            if entry.path().to_str().unwrap().ends_with(".service") {
                trace!("{:?}", entry.path());
                *last_id += 1;
                services.insert(*last_id, parse_service(&entry.path(), *last_id));
            }
        }
    }
}

pub fn parse_all_sockets(
    sockets: &mut std::collections::HashMap<InternalId, Unit>,
    path: &PathBuf,
    last_id: &mut InternalId,
) {
    let mut files: Vec<_> = std::fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    files.sort_by(|l, r| l.path().cmp(&r.path()));
    for entry in files {
        if entry.path().is_dir() {
            parse_all_sockets(sockets, path, last_id);
        } else {
            if entry.path().to_str().unwrap().ends_with(".socket") {
                trace!("{:?}", entry.path());
                *last_id += 1;
                sockets.insert(*last_id, parse_socket(&entry.path(), *last_id).unwrap());
            }
        }
    }
}
