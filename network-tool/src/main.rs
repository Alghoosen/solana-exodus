extern crate byte_unit;

use byte_unit::Byte;
use clap::{crate_description, crate_name, crate_version, value_t_or_exit, App, Arg, SubCommand};
use serde::export::fmt::Error;
use serde::export::Formatter;
use serde::{Deserialize, Serialize};
use serde_json::{self};
use std::collections::HashMap;
use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
struct LogLine {
    a: String,
    b: String,
    a_to_b: String,
    b_to_a: String,
}

impl Debug for LogLine {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        let a_to_b = Byte::from_str(&self.a_to_b).expect("Failed to read bytes");
        let b_to_a = Byte::from_str(&self.b_to_a).expect("Failed to read bytes");
        write!(
            f,
            "{{ \"{}\", \"{}\", {}, {} }}",
            self.a,
            self.b,
            a_to_b.get_bytes(),
            b_to_a.get_bytes()
        )
    }
}

fn main() {
    solana_logger::setup();

    let matches = App::new(crate_name!())
        .about(crate_description!())
        .version(crate_version!())
        .arg(
            Arg::with_name("iftop")
                .short("i")
                .long("iftop")
                .value_name("iftop log file")
                .takes_value(true)
                .help("Location of the log file generated by iftop"),
        )
        .subcommand(
            SubCommand::with_name("map-IP")
                .about("Public IP Address")
                .arg(
                    Arg::with_name("priv")
                        .long("priv")
                        .value_name("IP Address")
                        .takes_value(true)
                        .required(true)
                        .help("The private IP address that should be mapped"),
                )
                .arg(
                    Arg::with_name("pub")
                        .long("pub")
                        .value_name("IP Address")
                        .takes_value(true)
                        .required(true)
                        .help("The public IP address"),
                ),
        )
        .get_matches();

    let map_address;
    let private_address;
    let public_address;
    match matches.subcommand() {
        ("map-IP", Some(args_matches)) => {
            map_address = true;
            if let Some(addr) = args_matches.value_of("priv") {
                private_address = addr;
            } else {
                panic!("Private IP address must be provided");
            };
            if let Some(addr) = args_matches.value_of("pub") {
                public_address = addr;
            } else {
                panic!("Private IP address must be provided");
            };
        }
        _ => {
            map_address = false;
            private_address = "";
            public_address = "";
        }
    };

    let log_path = PathBuf::from(value_t_or_exit!(matches, "iftop", String));
    let mut log = fs::read_to_string(&log_path).expect("Unable to read log file");
    log.insert(0, '[');
    let terminate_at = log.rfind('}').expect("Didn't find a terminating '}'") + 1;
    let _ = log.split_off(terminate_at);
    log.push(']');
    let json_log: Vec<LogLine> = serde_json::from_str(&log).expect("Failed to parse log as JSON");

    let mut unique_latest_logs = HashMap::new();

    json_log.into_iter().rev().for_each(|l| {
        let key = (l.a.clone(), l.b.clone());
        unique_latest_logs.entry(key).or_insert(l);
    });

    println!(
        "{:#?}",
        unique_latest_logs
            .into_iter()
            .map(|(_, l)| {
                if map_address {
                    LogLine {
                        a: l.a.replace(private_address, public_address),
                        b: l.b.replace(private_address, public_address),
                        a_to_b: l.a_to_b,
                        b_to_a: l.b_to_a,
                    }
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
    );
}
