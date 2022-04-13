//! DoS tool
//!
//! Sends requests to cluster in a loop to measure
//! the effect of handling these requests on the performance of the cluster.
//!
//! * `mode` argument defines interface to use (e.g. rpc, tvu, tpu)
//! * `data-type` argument specifies the type of the request.
//! Some request types might be used only with particular `mode` value.
//! For example, `get-account-info` is valid only with `mode=rpc`.
//!
//! Most options are provided for `data-type = transaction`.
//! These options allow to compose transaction which fails at
//! a particular stage of the processing pipeline.
//!
//! To limit the number of possible options and simplify the usage of the tool,
//! The following configurations are suggested:
//! Let `COMMON="--mode tpu --data-type transaction --unique-transactions"`
//! 1. Without blockhash and payer:
//! 1.1 With invalid signatures
//! ```bash
//! solana-dos $COMMON --num-signatures 8
//! ```
//! 1.2 With valid signatures
//! ```bash
//! solana-dos $COMMON --valid-signatures --num-signatures 8
//! ```
//! 2. With blockhash and payer:
//! 2.1 Single instruction transaction
//! ```bash
//! solana-dos $COMMON --valid-blockhash --transaction-type single-transfer
//! ```
//! 2.2 Multi instruction transaction
//! ```bash
//! solana-dos $COMMON --valid-blockhash --transaction-type multi-transfer
//! ```
//! 2.3 Account creation transaction
//! ```bash
//! solana-dos $COMMON --valid-blockhash --transaction-type account-creation
//! ```
//!
#![allow(clippy::integer_arithmetic)]

use {
    crossbeam_channel::{select, tick, unbounded, Receiver, Sender},
    itertools::Itertools,
    log::*,
    rand::{thread_rng, Rng},
    solana_bench_tps::bench::{airdrop_lamports, generate_and_fund_keypairs},
    solana_client::rpc_client::RpcClient,
    //solana_client::transaction_executor::TransactionExecutor,
    solana_core::serve_repair::RepairProtocol,
    solana_dos::cli::*,
    solana_gossip::{
        contact_info::ContactInfo,
        gossip_service::{discover, discover_cluster, get_multi_client},
    },
    solana_sdk::{
        client::Client,
        hash::Hash,
        instruction::CompiledInstruction,
        message::Message,
        pubkey::Pubkey,
        signature::{Keypair, Signature, Signer},
        stake, system_instruction,
        system_instruction::SystemInstruction,
        system_program, system_transaction,
        transaction::Transaction,
    },
    solana_streamer::socket::SocketAddrSpace,
    std::{
        net::{SocketAddr, UdpSocket},
        process::exit,
        str::FromStr,
        sync::Arc,
        thread,
        time::{Duration, Instant},
    },
};

static REPORT_EACH_MILLIS: u128 = 10_000;
fn compute_tps(count: usize) -> usize {
    (count * 1000) / (REPORT_EACH_MILLIS as usize)
}

fn get_repair_contact(nodes: &[ContactInfo]) -> ContactInfo {
    let source = thread_rng().gen_range(0, nodes.len());
    let mut contact = nodes[source].clone();
    contact.id = solana_sdk::pubkey::new_rand();
    contact
}

#[derive(Clone)]
struct TransactionGenerator {
    blockhash: Hash,
    last_generated: Instant,
    transaction_params: TransactionParams,
}

/// Provides functionality to generate several types of transactions:
///
/// 1. Without blockhash
/// 1.1 With valid signatures (number of signatures is configurable)
/// 1.2 With invalid signatures (number of signatures is configurable)
///
/// 2. With blockhash (but still invalid due to high amount to transfer):
/// 2.1 Transfer payer -> destination (1 instruction per transaction)
/// 2.2 Transfer payer -> multiple destinations (many instructions per transaction)
/// 2.3 Create account transaction
///
impl TransactionGenerator {
    fn new(transaction_params: TransactionParams) -> Self {
        TransactionGenerator {
            blockhash: Hash::default(),
            last_generated: (Instant::now() - Duration::from_secs(100)),
            transaction_params,
        }
    }

    fn generate<T: 'static + Client + Send + Sync>(
        &mut self,
        payer: &Option<Keypair>,
        kpvals: Option<Vec<&Keypair>>, // provided for valid signatures
        client: &Option<Arc<T>>,
    ) -> Transaction {
        if self.transaction_params.valid_blockhash {
            // kpvals must be Some and contain at least one element
            if kpvals.is_none() || kpvals.as_ref().unwrap().len() == 0 {
                panic!("Expected at least one destination keypair to create transaction");
            }
            let client = client.as_ref().unwrap();
            let kpvals = kpvals.unwrap();
            let payer = payer.as_ref().unwrap();
            self.generate_with_blockhash(payer, kpvals, &client)
        } else {
            self.generate_without_blockhash(kpvals)
        }
    }

    fn generate_with_blockhash<T: 'static + Client + Send + Sync>(
        &mut self,
        payer: &Keypair,
        destinations: Vec<&Keypair>,
        client: &Arc<T>,
    ) -> Transaction {
        // generate a new blockhash every 1sec
        if self.transaction_params.valid_blockhash
            && self.last_generated.elapsed().as_millis() > 1000
        {
            self.blockhash = client.get_latest_blockhash().unwrap();
            self.last_generated = Instant::now();
        }

        // this argument must be present because it is required if blockhash is valid in cli
        let transaction_type = self.transaction_params.transaction_type.as_ref().unwrap();
        match transaction_type {
            TransactionType::SingleTransfer => {
                self.create_single_transfer_transaction(payer, &destinations[0].pubkey())
            }
            TransactionType::MultiTransfer => {
                self.create_multi_transfer_transaction(payer, &destinations)
            }
            TransactionType::AccountCreation => {
                self.create_account_transaction(payer, destinations[0])
            }
        }
    }

    /// Creates a transaction which transfers some lamports from payer to destination
    fn create_single_transfer_transaction(&self, payer: &Keypair, to: &Pubkey) -> Transaction {
        let to_transfer = 500_000_000; // specify amount which will cause error
        system_transaction::transfer(payer, to, to_transfer, self.blockhash)
    }

    /// Creates a transaction which transfers some lamports from payer to several destinations
    fn create_multi_transfer_transaction(
        &self,
        payer: &Keypair,
        to: &Vec<&Keypair>,
    ) -> Transaction {
        let to_transfer: u64 = 500_000_000; // specify amount which will cause error
        let to: Vec<(Pubkey, u64)> = to.iter().map(|to| (to.pubkey(), to_transfer)).collect();
        let instructions = system_instruction::transfer_many(&payer.pubkey(), to.as_slice());
        let message = Message::new(&instructions, Some(&payer.pubkey()));
        let mut tx = Transaction::new_unsigned(message);
        tx.sign(&[payer], self.blockhash);
        tx
    }

    /// Creates a transaction which opens account
    fn create_account_transaction(&self, payer: &Keypair, to: &Keypair) -> Transaction {
        let program_id = system_program::id(); // some valid program id
        let balance = 500_000_000;
        let space = 1024;
        let instructions = vec![system_instruction::create_account(
            &payer.pubkey(),
            &to.pubkey(),
            balance,
            space,
            &program_id,
        )];

        let message = Message::new(&instructions, Some(&payer.pubkey()));
        let signers: Vec<&Keypair> = vec![payer, to];
        Transaction::new(&signers, message, self.blockhash)
    }

    fn generate_without_blockhash(
        &mut self,
        kpvals: Option<Vec<&Keypair>>, // provided for valid signatures
    ) -> Transaction {
        // create an arbitrary valid instruction
        let lamports = 5;
        let transfer_instruction = SystemInstruction::Transfer { lamports };
        let program_ids = vec![system_program::id(), stake::program::id()];
        let instructions = vec![CompiledInstruction::new(
            0,
            &transfer_instruction,
            vec![0, 1],
        )];

        if self.transaction_params.valid_signatures {
            // Since we don't provide a payer, this transaction will end up
            // filtered at legacy.rs sanitize method (banking_stage) with error "a program cannot be payer"
            let keypairs = kpvals.unwrap();
            Transaction::new_with_compiled_instructions(
                &keypairs,
                &[],
                self.blockhash,
                program_ids,
                instructions,
            )
        } else {
            // Since we provided invalid signatures
            // this transaction will end up filtered at legacy.rs (banking_stage) because
            // num_required_signatures == 0
            let mut tx = Transaction::new_with_compiled_instructions(
                &[] as &[&Keypair; 0],
                &[],
                self.blockhash,
                program_ids,
                instructions,
            );
            tx.signatures = vec![Signature::new_unique(); self.transaction_params.num_signatures];
            tx
        }
    }
}

// Multithreading-related functions
//
// The most computationally expensive work is signing new
// transactions. So we generate them in n threads.
// Sending transactions is at least x8 times cheaper operation
// so we use only one thread for that for now:
//
// |TxGenerator|{n} -> |Tx channel|{1} -> |Sender|{1}
enum TransactionMsg {
    Transaction(Transaction),
    Exit,
}

fn create_sender_thread(
    tx_receiver: Receiver<TransactionMsg>,
    mut n_alive_threads: usize,
    target: &SocketAddr,
    //addr: SocketAddr,
) -> thread::JoinHandle<()> {
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    let target = target.clone();
    let timer_receiver = tick(Duration::from_millis(REPORT_EACH_MILLIS as u64));

    //let executor = TransactionExecutor::new(addr);

    thread::spawn(move || {
        let mut count: usize = 0;
        let mut total_count: usize = 0;
        let mut error_count = 0;
        let start_total = Instant::now();
        loop {
            select! {
                recv(tx_receiver) -> msg => {
                    match msg {
                        Ok(TransactionMsg::Transaction(tx)) => {
                            let data = bincode::serialize(&tx).unwrap();
                            let res = socket.send_to(&data, target);
                            if res.is_err() {
                                error_count += 1;
                            }
                            //executor.push_transactions(vec![tx]);
                            //let _ = executor.drain_cleared();

                            count += 1;
                            total_count += 1;
                        }
                        Ok(TransactionMsg::Exit) => {
                            info!("Worker is done");
                            n_alive_threads -= 1;
                            if n_alive_threads == 0 {
                                let t = start_total.elapsed().as_micros() as f64;
                                info!("Stopping sender. Count: {}, errors count: {}, total time: {}s, tps: {}",
                                    total_count,
                                    error_count,
                                    t / 1e6,
                                    ((count as f64)*1e6) / t,
                                );

                                break;
                            }
                        }
                        _ => panic!("Sender panics"),
                    }
                },
                recv(timer_receiver) -> _ => {
                    let t = start_total.elapsed().as_micros() as f64;
                    info!("Count: {}, tps: {}, time: {}",
                        count,
                        compute_tps(count),
                        t / 1e6,
                    );
                    count = 0;
                }
            }
        }
    })
}

// TODO use Measure struct from bench instead of manual measuring
fn create_generator_thread<T: 'static + Client + Send + Sync>(
    tx_sender: &Sender<TransactionMsg>,
    max_iter_per_thread: usize,
    transaction_generator: &mut TransactionGenerator,
    client: &Option<Arc<T>>,
    payer: Option<Keypair>,
) -> thread::JoinHandle<()> {
    let tx_sender = tx_sender.clone();
    let mut transaction_generator = transaction_generator.clone();
    let client = client.clone();

    let num_signatures = transaction_generator.transaction_params.num_signatures;
    let valid_signatures = transaction_generator.transaction_params.valid_signatures;
    let valid_blockhash = transaction_generator.transaction_params.valid_blockhash;

    // Generate n=1000 unique keypairs, which are used to create
    // chunks of keypairs.
    // The number of chunks is described by binomial coefficient
    // and hence 1000 seems to be a reasonable choice
    let mut keypairs_flat: Vec<Keypair> = Vec::new();
    if valid_signatures || valid_blockhash {
        keypairs_flat = (0..1000 * num_signatures).map(|_| Keypair::new()).collect();
    }

    thread::spawn(move || {
        let indexes: Vec<usize> = (0..keypairs_flat.len()).collect();
        let mut it = indexes.iter().permutations(num_signatures);
        let mut cnt = 0;
        let mut generation_elapsed: u64 = 0;
        loop {
            let generation_start = Instant::now();
            let chunk_keypairs = if valid_signatures || valid_blockhash {
                let permut = it.next();
                if permut.is_none() {
                    // if ran out of permutations, regenerate keys
                    keypairs_flat.iter_mut().for_each(|v| *v = Keypair::new());
                    info!("Regenerate keypairs");
                    continue;
                }
                let permut = permut.unwrap();
                Some(apply_permutation(permut, &keypairs_flat))
            } else {
                None
            };

            let tx = transaction_generator.generate(&payer, chunk_keypairs, &client);
            generation_elapsed =
                generation_elapsed.saturating_add(generation_start.elapsed().as_micros() as u64);

            let _ = tx_sender.send(TransactionMsg::Transaction(tx));
            cnt += 1;
            if max_iter_per_thread != 0 && cnt >= max_iter_per_thread {
                let _ = tx_sender.send(TransactionMsg::Exit);
                break;
            }
        }
        info!(
            "Finished thread. count = {}, avg generation time = {}, tps = {}",
            cnt,
            generation_elapsed / 1_000,
            (cnt as f64) / (generation_elapsed as f64) * 1e6
        );
    })
}

fn get_target_and_client(
    nodes: &[ContactInfo],
    mode: Mode,
    entrypoint_addr: SocketAddr,
) -> (Option<SocketAddr>, Option<RpcClient>) {
    let mut target = None;
    let mut rpc_client = None;
    if nodes.is_empty() {
        if mode == Mode::Rpc {
            rpc_client = Some(RpcClient::new_socket(entrypoint_addr));
        }
        target = Some(entrypoint_addr);
    } else {
        info!("************ NODE ***********");
        for node in nodes {
            info!("{:?}", node);
        }
        info!("ADDR = {}", entrypoint_addr);

        for node in nodes {
            if node.gossip == entrypoint_addr {
                info!("{}", node.gossip);
                target = match mode {
                    Mode::Gossip => Some(node.gossip),
                    Mode::Tvu => Some(node.tvu),
                    Mode::TvuForwards => Some(node.tvu_forwards),
                    Mode::Tpu => {
                        rpc_client = Some(RpcClient::new_socket(node.rpc));
                        Some(node.tpu)
                    }
                    Mode::TpuForwards => Some(node.tpu_forwards),
                    Mode::Repair => Some(node.repair),
                    Mode::ServeRepair => Some(node.serve_repair),
                    Mode::Rpc => {
                        rpc_client = Some(RpcClient::new_socket(node.rpc));
                        None
                    }
                };
                break;
            }
        }
    }
    (target, rpc_client)
}

fn run_dos_rpc_mode(
    rpc_client: Option<RpcClient>,
    iterations: usize,
    data_type: DataType,
    data_input: Option<String>,
) {
    let mut last_log = Instant::now();
    let mut total_count: usize = 0;
    let mut count = 0;
    let mut error_count = 0;
    loop {
        match data_type {
            DataType::GetAccountInfo => {
                let res = rpc_client
                    .as_ref()
                    .unwrap()
                    .get_account(&Pubkey::from_str(data_input.as_ref().unwrap()).unwrap());
                if res.is_err() {
                    error_count += 1;
                }
            }
            DataType::GetProgramAccounts => {
                let res = rpc_client
                    .as_ref()
                    .unwrap()
                    .get_program_accounts(&Pubkey::from_str(data_input.as_ref().unwrap()).unwrap());
                if res.is_err() {
                    error_count += 1;
                }
            }
            _ => {
                panic!("unsupported data type");
            }
        }
        count += 1;
        total_count += 1;
        if last_log.elapsed().as_millis() > REPORT_EACH_MILLIS {
            info!(
                "count: {}, errors: {}, tps: {}",
                count,
                error_count,
                compute_tps(count)
            );
            last_log = Instant::now();
            count = 0;
        }
        if iterations != 0 && total_count >= iterations {
            break;
        }
    }
}

fn apply_permutation<'a, T>(indexes: Vec<&usize>, items: &'a Vec<T>) -> Vec<&'a T> {
    let mut res = Vec::with_capacity(indexes.len());
    for i in indexes {
        res.push(&items[*i]);
    }
    res
}

fn run_dos_transactions<T: 'static + Client + Send + Sync>(
    //addr: SocketAddr,
    target: SocketAddr,
    iterations: usize,
    client: Option<Arc<T>>,
    faucet_addr: Option<SocketAddr>,
    transaction_params: TransactionParams,
) {
    info!("{:?}", transaction_params);
    let num_gen_threads = transaction_params.num_gen_threads;
    let mut transaction_generator = TransactionGenerator::new(transaction_params);

    let (tx_sender, tx_receiver) = unbounded();

    let sender_thread = create_sender_thread(tx_receiver, num_gen_threads, &target); //, addr);

    let max_iter_per_thread = iterations / num_gen_threads;

    // The assumption is that if we use valid blockhash, we also have a payer
    let payers: Vec<Option<Keypair>> = if transaction_generator.transaction_params.valid_blockhash {
        // create a new payer for each thread since Keypair is not clonable
        // each payer is used to fund transaction
        // transactions are built to be invalid so the the amount here is arbitrary
        let funding_key = Keypair::new();
        let funding_key = Arc::new(funding_key);
        let res = generate_and_fund_keypairs(
            client.as_ref().unwrap().clone(),
            faucet_addr,
            &funding_key,
            num_gen_threads,
            1_000_000,
        );
        match res {
            Ok(r) => r.into_iter().map(|keypair| Some(keypair)).collect(),
            Err(e) => {
                panic!("ERROR {:?}", e);
                Vec::new()
            }
        }
    } else {
        std::iter::repeat_with(|| None)
            .take(num_gen_threads)
            .collect()
    };

    let tx_generator_threads: Vec<_> = payers
        .into_iter()
        .map(|payer| {
            create_generator_thread(
                &tx_sender,
                max_iter_per_thread,
                &mut transaction_generator,
                &client,
                payer,
            )
        })
        .collect();

    if let Err(err) = sender_thread.join() {
        println!("join() failed with: {:?}", err);
    }
    for t_generator in tx_generator_threads {
        if let Err(err) = t_generator.join() {
            println!("join() failed with: {:?}", err);
        }
    }
    println!("This is the end");
}

fn run_dos<T: 'static + Client + Send + Sync>(
    nodes: &[ContactInfo],
    iterations: usize,
    client: Option<Arc<T>>,
    params: DosClientParameters,
) {
    let (target, rpc_client) = get_target_and_client(nodes, params.mode, params.entrypoint_addr);
    let target = target.expect("should have target");
    info!("Targeting {}", target);

    if params.mode == Mode::Rpc {
        run_dos_rpc_mode(rpc_client, iterations, params.data_type, params.data_input);
    } else if params.data_type == DataType::Transaction
        && params.transaction_params.unique_transactions
    {
        //let addr = nodes[0].rpc;
        run_dos_transactions(
            //addr,
            target,
            iterations,
            client,
            params.faucet_addr,
            params.transaction_params,
        );
    } else {
        let mut data = match params.data_type {
            DataType::RepairHighest => {
                let slot = 100;
                let req =
                    RepairProtocol::WindowIndexWithNonce(get_repair_contact(nodes), slot, 0, 0);
                bincode::serialize(&req).unwrap()
            }
            DataType::RepairShred => {
                let slot = 100;
                let req = RepairProtocol::HighestWindowIndexWithNonce(
                    get_repair_contact(nodes),
                    slot,
                    0,
                    0,
                );
                bincode::serialize(&req).unwrap()
            }
            DataType::RepairOrphan => {
                let slot = 100;
                let req = RepairProtocol::OrphanWithNonce(get_repair_contact(nodes), slot, 0);
                bincode::serialize(&req).unwrap()
            }
            DataType::Random => {
                vec![0; params.data_size]
            }
            DataType::Transaction => {
                let tp = params.transaction_params;
                info!("{:?}", tp);

                let funding_key: Option<Keypair> = if tp.valid_blockhash {
                    let funding_key = Keypair::new();

                    let total = 1_000_000;
                    let client = client.as_ref().unwrap();
                    if client.get_balance(&funding_key.pubkey()).unwrap_or(0) < total {
                        let r = airdrop_lamports(
                            client.as_ref(),
                            params.faucet_addr.as_ref().unwrap(),
                            &funding_key,
                            total,
                        );
                        match r {
                            Ok(_) => {}
                            Err(error) => panic!("Airdrop failed with error: {:?}", error),
                        }
                    }
                    Some(funding_key)
                } else {
                    None
                };

                let keypairs: Vec<Keypair> =
                    (0..tp.num_signatures).map(|_| Keypair::new()).collect();
                let keypairs_chunk: Option<Vec<&Keypair>> =
                    if tp.valid_signatures || tp.valid_blockhash {
                        Some(keypairs.iter().map(|kp| kp).collect())
                    } else {
                        None
                    };

                let mut transaction_generator = TransactionGenerator::new(tp);
                let tx = transaction_generator.generate(&funding_key, keypairs_chunk, &client);
                info!("{:?}", tx);
                bincode::serialize(&tx).unwrap()
            }
            _ => panic!("Unsupported data_type detected"),
        };

        let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
        let mut last_log = Instant::now();
        let mut total_count: usize = 0;
        let mut count: usize = 0;
        let mut error_count = 0;
        loop {
            if params.data_type == DataType::Random {
                thread_rng().fill(&mut data[..]);
            }
            let res = socket.send_to(&data, target);
            if res.is_err() {
                error_count += 1;
            }
            count += 1;
            total_count += 1;
            if last_log.elapsed().as_millis() > REPORT_EACH_MILLIS {
                info!(
                    "count: {}, errors: {}, tps: {}",
                    count,
                    error_count,
                    compute_tps(count)
                );
                last_log = Instant::now();
                count = 0;
            }
            if iterations != 0 && total_count >= iterations {
                break;
            }
        }
    }
}

fn main() {
    solana_logger::setup_with_default("solana=info");
    let cmd_params = build_cli_parameters();

    let mut nodes = vec![];
    if !cmd_params.skip_gossip {
        info!("Finding cluster entry: {:?}", cmd_params.entrypoint_addr);
        let socket_addr_space = SocketAddrSpace::new(cmd_params.allow_private_addr);
        let (gossip_nodes, _validators) = discover(
            None, // keypair
            Some(&cmd_params.entrypoint_addr),
            None,                              // num_nodes
            Duration::from_secs(60),           // timeout
            None,                              // find_node_by_pubkey
            Some(&cmd_params.entrypoint_addr), // find_node_by_gossip_addr
            None,                              // my_gossip_addr
            0,                                 // my_shred_version
            socket_addr_space,
        )
        .unwrap_or_else(|err| {
            eprintln!(
                "Failed to discover {} node: {:?}",
                cmd_params.entrypoint_addr, err
            );
            exit(1);
        });
        nodes = gossip_nodes;
    }

    info!("done found {} nodes", nodes.len());

    // create client which is used for airdrop and get blockhash
    let client = if cmd_params.transaction_params.valid_blockhash {
        let num_nodes = 1;
        info!("Connecting to the cluster");
        let nodes = discover_cluster(
            &cmd_params.entrypoint_addr,
            num_nodes,
            SocketAddrSpace::Unspecified,
        )
        .unwrap_or_else(|err| {
            eprintln!("Failed to discover {} nodes: {:?}", num_nodes, err);
            exit(1);
        });

        let (client, num_clients) = get_multi_client(&nodes, &SocketAddrSpace::Unspecified);
        if nodes.len() < num_clients {
            eprintln!(
                "Error: Insufficient nodes discovered.  Expecting {} or more",
                nodes.len()
            );
            exit(1);
        }
        Some(Arc::new(client))
    } else {
        None
    };
    run_dos(&nodes, 0, client, cmd_params);
}

#[cfg(test)]
pub mod test {
    use solana_client::udp_client::UdpTpuConnection;

    use {
        super::*,
        solana_client::thin_client::create_client,
        solana_client::thin_client::ThinClient,
        solana_core::validator::ValidatorConfig,
        solana_faucet::faucet::run_local_faucet,
        solana_gossip::cluster_info::VALIDATOR_PORT_RANGE,
        solana_local_cluster::{
            cluster::Cluster,
            local_cluster::{ClusterConfig, LocalCluster},
            validator_configs::make_identical_validator_configs,
        },
        solana_sdk::timing::timestamp,
    };

    // thin wrapper for the run_dos function
    // to avoid specifying everywhere generic parameters
    fn run_dos_no_client(nodes: &[ContactInfo], iterations: usize, params: DosClientParameters) {
        run_dos::<ThinClient<UdpTpuConnection>>(nodes, iterations, None, params);
    }

    #[test]
    fn test_dos() {
        let nodes = [ContactInfo::new_localhost(
            &solana_sdk::pubkey::new_rand(),
            timestamp(),
        )];
        let entrypoint_addr = nodes[0].gossip;

        run_dos_no_client(
            &nodes,
            1,
            DosClientParameters {
                entrypoint_addr,
                faucet_addr: None,
                mode: Mode::Tvu,
                data_size: 10,
                data_type: DataType::Random,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams::default(),
            },
        );

        run_dos_no_client(
            &nodes,
            1,
            DosClientParameters {
                entrypoint_addr,
                faucet_addr: None,
                mode: Mode::Repair,
                data_size: 10,
                data_type: DataType::RepairHighest,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams::default(),
            },
        );

        run_dos_no_client(
            &nodes,
            1,
            DosClientParameters {
                entrypoint_addr,
                faucet_addr: None,
                mode: Mode::ServeRepair,
                data_size: 10,
                data_type: DataType::RepairShred,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams::default(),
            },
        );
    }

    #[test]
    #[ignore]
    fn test_dos_random() {
        solana_logger::setup();
        let num_nodes = 1;
        let cluster =
            LocalCluster::new_with_equal_stakes(num_nodes, 100, 3, SocketAddrSpace::Unspecified);
        assert_eq!(cluster.validators.len(), num_nodes);

        let nodes = cluster.get_node_pubkeys();
        let node = cluster.get_contact_info(&nodes[0]).unwrap().clone();
        let nodes_slice = [node];

        // send random transactions to TPU
        // will be discarded on sigverify stage
        run_dos_no_client(
            &nodes_slice,
            1000,
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                faucet_addr: None,
                mode: Mode::Tpu,
                data_size: 1024,
                data_type: DataType::Random,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams::default(),
            },
        );
    }

    #[test]
    #[ignore]
    fn test_dos_without_blockhash() {
        solana_logger::setup();
        let num_nodes = 1;
        let cluster =
            LocalCluster::new_with_equal_stakes(num_nodes, 100, 3, SocketAddrSpace::Unspecified);
        assert_eq!(cluster.validators.len(), num_nodes);

        let nodes = cluster.get_node_pubkeys();
        let node = cluster.get_contact_info(&nodes[0]).unwrap().clone();
        let nodes_slice = [node];

        let client = Arc::new(create_client(
            (cluster.entry_point_info.rpc, cluster.entry_point_info.tpu),
            VALIDATOR_PORT_RANGE,
        ));

        // creates one transaction with 8 valid signatures and sends it 10 times
        // this is done in single thread
        run_dos(
            &nodes_slice,
            10,
            Some(client.clone()),
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                faucet_addr: None,
                mode: Mode::Tpu,
                data_size: 0, // irrelevant
                data_type: DataType::Transaction,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams {
                    num_signatures: 8,
                    valid_blockhash: false,
                    valid_signatures: true,
                    unique_transactions: false,
                    num_gen_threads: 1,
                    transaction_type: None,
                },
            },
        );

        // creates and sends unique transactions which has invalid signatures
        // it uses several threads
        run_dos(
            &nodes_slice,
            10,
            Some(client.clone()),
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                faucet_addr: None,
                mode: Mode::Tpu,
                data_size: 0, // irrelevant
                data_type: DataType::Transaction,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams {
                    num_signatures: 8,
                    valid_blockhash: false,
                    valid_signatures: false,
                    unique_transactions: true,
                    num_gen_threads: 4,
                    transaction_type: None,
                },
            },
        );

        // creates and sends unique transactions which has valid signatures
        // it uses several threads
        run_dos(
            &nodes_slice,
            10,
            Some(client.clone()),
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                faucet_addr: None,
                mode: Mode::Tpu,
                data_size: 0, // irrelevant
                data_type: DataType::Transaction,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams {
                    num_signatures: 8,
                    valid_blockhash: false,
                    valid_signatures: true,
                    unique_transactions: true,
                    num_gen_threads: 4,
                    transaction_type: None,
                },
            },
        );
    }

    #[test]
    #[ignore]
    fn test_dos_with_blockhash_and_payer() {
        solana_logger::setup();
        let num_nodes = 1;
        let native_instruction_processors = vec![];
        let cluster = LocalCluster::new(
            &mut ClusterConfig {
                node_stakes: vec![999_990; num_nodes],
                cluster_lamports: 200_000_000,
                validator_configs: make_identical_validator_configs(
                    &ValidatorConfig::default_for_test(),
                    num_nodes,
                ),
                native_instruction_processors,
                ..ClusterConfig::default()
            },
            SocketAddrSpace::Unspecified,
        );
        assert_eq!(cluster.validators.len(), num_nodes);

        // 1. Transfer funds to faucet account
        // 2. Create faucet thread
        // 3. Fund funding_key using faucet
        // 4. Transfer required funds from funding_key account to newly created accounts
        let faucet_keypair = Keypair::new();
        cluster.transfer(
            &cluster.funding_keypair,
            &faucet_keypair.pubkey(),
            100_000_000,
        );
        let faucet_addr = run_local_faucet(faucet_keypair, None);

        let nodes = cluster.get_node_pubkeys();
        let node = cluster.get_contact_info(&nodes[0]).unwrap().clone();
        let nodes_slice = [node];

        let client = Arc::new(create_client(
            (cluster.entry_point_info.rpc, cluster.entry_point_info.tpu),
            VALIDATOR_PORT_RANGE,
        ));

        // creates one transaction and sends it 10 times
        // this is done in single thread
        run_dos(
            &nodes_slice,
            10,
            Some(client.clone()),
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                faucet_addr: Some(faucet_addr),
                mode: Mode::Tpu,
                data_size: 0, // irrelevant if not random
                data_type: DataType::Transaction,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams {
                    num_signatures: 2,
                    valid_blockhash: true,
                    valid_signatures: true,
                    unique_transactions: false,
                    num_gen_threads: 1,
                    transaction_type: Some(TransactionType::SingleTransfer),
                },
            },
        );
        // creates and sends unique transactions of type SingleTransfer
        // which tries to send too much lamports from payer to one recipient
        // it uses several threads
        run_dos(
            &nodes_slice,
            10,
            Some(client.clone()),
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                faucet_addr: Some(faucet_addr),
                mode: Mode::Tpu,
                data_size: 0, // irrelevant if not random
                data_type: DataType::Transaction,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams {
                    num_signatures: 2,
                    valid_blockhash: true,
                    valid_signatures: true,
                    unique_transactions: true,
                    num_gen_threads: 4,
                    transaction_type: Some(TransactionType::SingleTransfer),
                },
            },
        );
        // creates and sends unique transactions of type MultiTransfer
        // which tries to send too much lamports from payer to several recipients
        // it uses several threads
        run_dos(
            &nodes_slice,
            10,
            Some(client.clone()),
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                faucet_addr: Some(faucet_addr),
                mode: Mode::Tpu,
                data_size: 0, // irrelevant if not random
                data_type: DataType::Transaction,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams {
                    num_signatures: 2,
                    valid_blockhash: true,
                    valid_signatures: true,
                    unique_transactions: true,
                    num_gen_threads: 4,
                    transaction_type: Some(TransactionType::MultiTransfer),
                },
            },
        );
        // creates and sends unique transactions of type CreateAccount
        // which tries to create account with too large balance
        // it uses several threads
        run_dos(
            &nodes_slice,
            10,
            Some(client.clone()),
            DosClientParameters {
                entrypoint_addr: cluster.entry_point_info.gossip,
                faucet_addr: Some(faucet_addr),
                mode: Mode::Tpu,
                data_size: 0, // irrelevant if not random
                data_type: DataType::Transaction,
                data_input: None,
                skip_gossip: false,
                allow_private_addr: false,
                transaction_params: TransactionParams {
                    num_signatures: 2,
                    valid_blockhash: true,
                    valid_signatures: true,
                    unique_transactions: true,
                    num_gen_threads: 4,
                    transaction_type: Some(TransactionType::AccountCreation),
                },
            },
        );
    }
}
