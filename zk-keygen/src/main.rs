use {
    bip39::{Mnemonic, MnemonicType, Seed},
    clap::{crate_description, crate_name, Arg, ArgMatches, Command},
    solana_clap_v3_utils::{
        input_parsers::STDOUT_OUTFILE_TOKEN,
        keygen::{
            check_for_overwrite,
            mnemonic::{acquire_language, acquire_passphrase_and_message, WORD_COUNT_ARG},
            no_outfile_arg, KeyGenerationCommonArgs, NO_OUTFILE_ARG,
        },
        DisplayError,
    },
    solana_cli_config::CONFIG_FILE,
    solana_sdk::signer::EncodableKey,
    solana_zk_token_sdk::encryption::{auth_encryption::AeKey, elgamal::ElGamalKeypair},
    std::error,
};

fn output_encodable_key<K: EncodableKey>(
    key: &K,
    outfile: &str,
    source: &str,
) -> Result<(), Box<dyn error::Error>> {
    if outfile == STDOUT_OUTFILE_TOKEN {
        let mut stdout = std::io::stdout();
        key.write(&mut stdout)?;
    } else {
        key.write_to_file(outfile)?;
        println!("Wrote {source} to {outfile}");
    }
    Ok(())
}

fn app(crate_version: &str) -> Command {
    Command::new(crate_name!())
        .about(crate_description!())
        .version(crate_version)
        .subcommand_required(true)
        .arg_required_else_help(true)
        .arg({
            let arg = Arg::new("config_file")
                .short('C')
                .long("config")
                .value_name("FILEPATH")
                .takes_value(true)
                .global(true)
                .help("Configuration file to use");
            if let Some(ref config_file) = *CONFIG_FILE {
                arg.default_value(config_file)
            } else {
                arg
            }
        })
        .subcommand(
            Command::new("new")
                .about("Generate a new encryption key/keypair file from a random seed phrase and optional BIP39 passphrase")
                .disable_version_flag(true)
                .arg(
                    Arg::new("key_type")
                        .short('t')
                        .long("key-type")
                        .takes_value(true)
                        .possible_values(["elgamal", "symmetric"])
                        .value_name("TYPE")
                        .required(true)
                        .help("The type of encryption key")
                )
                .arg(
                    Arg::new("outfile")
                        .short('o')
                        .long("outfile")
                        .value_name("FILEPATH")
                        .takes_value(true)
                        .help("Path to generated file"),
                )
                .arg(
                    Arg::new("force")
                        .short('f')
                        .long("force")
                        .help("Overwrite the output file if it exists"),
                )
                .arg(
                    Arg::new("silent")
                        .short('s')
                        .long("silent")
                        .help("Do not display seed phrase. Useful when piping output to other programs that prompt for user input, like gpg"),
                )
                .key_generation_common_args()
                .arg(no_outfile_arg().conflicts_with_all(&["outfile", "silent"]))
        )
}

fn main() -> Result<(), Box<dyn error::Error>> {
    let matches = app(solana_version::version!())
        .try_get_matches()
        .unwrap_or_else(|e| e.exit());
    do_main(&matches).map_err(|err| DisplayError::new_as_boxed(err).into())
}

fn do_main(matches: &ArgMatches) -> Result<(), Box<dyn error::Error>> {
    let subcommand = matches.subcommand().unwrap();
    match subcommand {
        ("new", matches) => {
            let key_type = match matches.value_of("key_type").unwrap() {
                "elgamal" => KeyType::ElGamal,
                "symmetric" => KeyType::Symmetric,
                _ => unreachable!(),
            };

            let mut path = dirs_next::home_dir().expect("home directory");
            let outfile = if matches.is_present("outfile") {
                matches.value_of("outfile")
            } else if matches.is_present(NO_OUTFILE_ARG.name) {
                None
            } else {
                path.extend([".config", "solana", &key_type.default_file_name()]);
                Some(path.to_str().unwrap())
            };

            match outfile {
                Some(STDOUT_OUTFILE_TOKEN) => (),
                Some(outfile) => check_for_overwrite(outfile, matches)?,
                None => (),
            }

            let word_count: usize = matches.value_of_t(WORD_COUNT_ARG.name).unwrap();
            let mnemonic_type = MnemonicType::for_word_count(word_count)?;
            let language = acquire_language(matches);

            let mnemonic = Mnemonic::new(mnemonic_type, language);
            let (passphrase, passphrase_message) = acquire_passphrase_and_message(matches).unwrap();
            let seed = Seed::new(&mnemonic, &passphrase);

            match key_type {
                KeyType::ElGamal => {
                    let silent = matches.is_present("silent");
                    if !silent {
                        println!("Generating a new ElGamal keypair");
                    }

                    let elgamal_keypair = ElGamalKeypair::from_seed(seed.as_bytes())?;
                    if let Some(outfile) = outfile {
                        output_encodable_key(&elgamal_keypair, outfile, "new ElGamal keypair")
                            .map_err(|err| format!("Unable to write {outfile}: {err}"))?;
                    }

                    if !silent {
                        let phrase: &str = mnemonic.phrase();
                        let divider = String::from_utf8(vec![b'='; phrase.len()]).unwrap();
                        println!(
                            "{}\npubkey: {}\n{}\nSave this seed phrase{} to recover your new ElGamal keypair:\n{}\n{}",
                            &divider, elgamal_keypair.public, &divider, passphrase_message, phrase, &divider
                        );
                    }
                }
                KeyType::Symmetric => {
                    let silent = matches.is_present("silent");
                    if !silent {
                        println!("Generating a new symmetric encryption key");
                    }

                    let symmetric_key = AeKey::from_seed(seed.as_bytes())?;
                    if let Some(outfile) = outfile {
                        output_encodable_key(&symmetric_key, outfile, "new symmetric key")
                            .map_err(|err| format!("Unable to write {outfile}: {err}"))?;
                    }

                    if !silent {
                        let phrase: &str = mnemonic.phrase();
                        let divider = String::from_utf8(vec![b'='; phrase.len()]).unwrap();
                        println!(
                            "{}\nSave this seed phrase{} to recover your new symmetric key:\n{}\n{}",
                            &divider, passphrase_message, phrase, &divider
                        );
                    }
                }
            }
        }
        _ => unreachable!(),
    }

    Ok(())
}

enum KeyType {
    ElGamal,
    Symmetric,
}

impl KeyType {
    fn default_file_name(&self) -> String {
        match self {
            KeyType::ElGamal => "elgamal.json".to_string(),
            KeyType::Symmetric => "symmetric.json".to_string(),
        }
    }
}
