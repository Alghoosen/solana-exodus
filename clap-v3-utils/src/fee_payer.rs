use {
    crate::{input_validators, ArgConstant},
    clap::Arg,
};

pub const FEE_PAYER_ARG: ArgConstant<'static> = ArgConstant {
    name: "fee_payer",
    long: "fee-payer",
    help: "Specify the fee-payer account. This may be a keypair file, the ASK keyword \nor the \
           pubkey of an offline signer, provided an appropriate --signer argument \nis also \
           passed. Defaults to the client keypair.",
};

pub fn fee_payer_arg<'a>() -> Arg<'a> {
    Arg::new(FEE_PAYER_ARG.name)
        .long(FEE_PAYER_ARG.long)
        .takes_value(true)
        .value_name("KEYPAIR")
        .validator(|s| input_validators::is_valid_signer(s))
        .help(FEE_PAYER_ARG.help)
}
