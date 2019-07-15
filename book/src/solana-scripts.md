# Solana Scripts

In Solana programs have absolute control over the state assigned to them.  The
goal of this architecture is provide a simple way for on-chain programs to
interact with other programs without violating the state ownership guarantees.

Contract composition has led to multiple multi-million dollar bugs in Ethereum

* https://www.parity.io/security-alert-2/

A few solutions that have been tried:

* Eos: message queues
* Ethereum: implementation delegation
* Libra: resources

The ideal solution to this problem is an infallible Trusted Third Party. For
example, clients that want to bet on an outcome of an on-chain game of
Tic-Tac-Toe, transfer their tokens to the TTP client, and the TTP transfers the
prize to the winner at the end of the game.

Scripts are on-chain programs that act as an infallible TTP client.

## Scripts

To implement an infallible TTP, scripts behave as on-chain clients, with an
on-chain wallet that has access to script unique keypairs that only the script
can sign with.  Additionally, like regular clients, scripts cannot directly
mutate state or transfer funds, and must do those things via calls to
instructions.

Scripts are executable bytecode that run in an environment with additional
features and restrictions than programs.

Scripts have the following additional features:
* `process_instruction`, a synchronous call to process an instructions.

A wallet with the following APIs:

* `keypair_pubkey`, generate a persistent key that only this script can sign with.
* `sign_instruction`, sign an instruction with a script key.

Since scripts are clients, they cannot:
* Directly mutate state, or directly move funds between accounts.

Scripts rely on programs to move funds or mutate state, and act purely as
clients.  All instructions that a script issues must succeed, or the entire
script fails.

## Secure Script Execution

Scripts execute as a single atomic transaction, either all the instructions the
script issued succeed, or the entire script fails.

While scripts execute as a single atomic unit, their execution may still choose
a dynamic path.  The execution path based on the current on-chain state.  The
state may not match the same state that clients observed when signing the
transaction.  This is a problem for regular programs as well.  But a single
program controls all the state transitions, and can define the rules for
composing out of order messages.  Since script execute multiple programs, any
change to any of the programs in the execution path could potentially change the
outcome of the script.

For example:

A script calls program A followed by B or C.  Alice signs a transaction to the
script, and Mallory signs a transaction for A only in such a way that causes the
script to change its execution path.

Several solutions exist to this problem:

* clients sign a transaction with a specific blockhash, if any of the state that
the script depends on have changed signed the signature the transaction fails.

This solution is problematic because the stable blockhash is the root, but the
transaction will be encoded at the head.  So the maximum throughput per script
is going to be limited by the speed at which each transaction finalizes with a
high confidence, which could be at least 10 blocks.

* clients sign the expected merkle root of the changes to state after the
transaction, such as the account updates.  If the changes fail to match, the
transaction fails.

This solution has better performance characteristics, but users must still
linearize all the transactions to the script externally, because changes cannot
be applied out of order.

This design proposes another approach.  Users know ahead of time which
instructions the script will generate, and if signatures are required for the
instructions.  During the script execution, calls to `process_instruction`
yield, and the next instruction to be processed is invoked.

The transaction invoked by the client must declare all the accounts that the
script will need up front and provide all the necessary client signatures, as
well as encode the instruction vector that the script will generate.

The latter provides clear authorization for the script to take actions on behalf
of the client, since each instruction specifies the clients keys and explicit
signatures for each explicit instruction.  Users do not need to guess which
instructions the script will execute, and authorize each one explicitly.  If the
on-chain state has changed such that the script will take a different branch and
execute a different instruction, then the script fails.  This allows the
programs to implement out of order composition and for users to take advantage
of those performance benefits.

Once the instruction is invoked, the script is resumed from the last point of
execution.

## Script Wallet Methods

A TTP needs the ability to create pubkeys and generate signatures for those
keys.  At script creation time the loader program may authorize specific
pubkeys that the script and only the script can sign with.  To ensure that these
keys cannot be signed by the client, the addresses are derived from a sha256 of
the script pubkey and the key index number.

* `pub fn keypair_pubkey(key_index: u64) -> Pubkey`

Retrivies the script keypair at index `key_index`.  The pubkey of the keypair is
`sha256.hash(program_id).hash(key_index)`, and therefore has no real private
key.  This keypair can only be used to sign messages with the `sign_instruction`
function by the script.

* `pub fn sign_instruction(ix: &mut Instruction, key_index: u64) -> ()`

Signs the message for the script pubkey that is generated with `key_index`.
Clients can generate these keys locally and encode them into the instruction
vector.  Only scripts can sign with these keys.  While a client is expected to
encode the key into the instruction vector, the execution of the scrip will call
`sign_instruction` and set the `KeyedAccounts::is_signer` flag.

## Script Instruction Execution Methods

Scripts cannot directly mutate state like programs, but they can execute any
program instructions.

* `fn process_instruction(ix: Instruction) -> Result<(), InstructionError>`

This method is available to scripts to execute an instruction in the runtime.

## Script Initialization

* `LoaderInstruction::FinalizeScript`

`LoaderInstruction::FinalizeScript` designates that the loaded executable
bytecode is a script, and creates a new instance of the script. The difference
between scripts and programs is that script execution yields to external program
instructions, and scripts have the capability to sign.  Scripts also cannot
modify any account data directly.

`FinalizeScript` may be called more than once on the same loaded bytecode to
create unique instances of scripts each with their own script wallet.

## Script Instruction Vector

In the (script example)[#Script Example] to execute a transaction that calls
`BetOnTicTacToeScriptInstruction::Initialize` Bob and Alice need to sign a
transaction with the following instruction vector

```
Message {
  instructions: vec![
    BetOnTicTacToeScriptInstruction::Initialize{...},    //the script
    SystemInstruction::Transfer{...},   //transfer alice's lamports to the script
    SystemInstruction::Transfer{...},   //transfer bob's lamports to the script
    SystemInstruction::Create{...},     //allocate the scripts data key
  ],
}
```

Both Bob and Alice must provide signatures for the Transfers. Since Bob and
Alice signed this transaction, they have authorized the script to perform the
following transfers.  The script execution succeeds if and only if the script
generates the exact same instruction vector during execution.  Bob and Alice
have no way to ensure what the state of any of the programs will be during the
start of the script. The explicit instruction vector ensures that the script
behaves as an infallible TTP.

* `SystemInstruction::Create{...}` - This instruction references the keys
generated by the script, and therefore cannot be signed by the user.  During the
script execution the script will add the signature to the instruction.

## Script Example

In this example, a script accepts tokens from two different accounts, and pays
out the total to whoever wins the game of Tic-Tac-Toe.

```
enum BetOnTicTacToeScript {
    Initialize {amount: u64, game: Pubkey},
    Claim,
};

// This is an additional program that implements some features that this script
// needs.  Scripts cannot mutate state directly, and rely on programs to handle
// state mutation.
const BetOnTicTacToeProgramId: Pubkey = [];
enum BetOnTicTacToeProgramInstruction {
    // copy the game id from to account 1
    StoreGame,
}

pub fn process_instruction(
    program_id: &Pubkey,
    keyed_accounts: &mut [KeyedAccount],
    data: &[u8],
) -> Result<(), InstructionError> {
    let cmd = deserialize(&data)?;
    match cmd {
        case BetOnTicTacToeScriptInstruction::Initialize{ amount, game} => {
            //The scripts system account to store lamports
            let script_tokens_key = script::keypair_pubkey(0);

            let from_alice = system_instruction::transfer(
                            keyed_accounts[1].key, //alice
                            script_tokens_key,
                            amount);

            //alice must have signed this instruction
            script::process_instruction(from_alice)?;
            let from_bob = system_instruction::transfer(
                            keyed_accounts[2].key, //bob
                            script_tokens_key,
                            amount);

            //bob must have signed this instruction
            script::process_instruction(from_bob)?;

            //The scripts `program_id` account to store the game state
            let script_data_key = script::keypair_pubkey(1);

            //to save the game, the data key needs to be allocated
            let mut create = system_instruction::create(
                            script_token_key,
                            script_data_key,
                            2,
                            size_of(game),
                            BetOnTicTacToeProgramId);
            script::sign_instruction(&mut create, 0);
            script::process_instruction(create)?;

            let prize = amount * 2 - 2;
            //call BetOnTicTacToeProgramId
            let store_game = betontictactoe::store_game(
                            script_data_key,
                            &serialize((prize, game)?);
            script::process_instruction(store_game)?;
        },
        case BetOnTicTacToeScriptInstruction::Claim => {
            //script pubkey 0 is always the same
            let script_tokens_key = script::keypair_pubkey(0);
            //script pubkey 1 is always the same
            let script_data_key = script::keypair_pubkey(1);

            //get the game key
            assert_eq!(script_data_key, keyed_accounts[1].key);
            let (prize, game_key) = deserialize(&keyed_accounts[1].account.data)?;

            //read the game
            assert_eq!(game_key, keyed_accounts[2].key);
            let game = deserialize(&keyed_accounts[2].account.data)?;

            assert!(game.is_over);

            //Ignoring ties for brevity 
            //transfer from the script to the winner of the game
            assert_eq!(script_tokens_key, keyed_accounts[3].key);
            let mut to_winner = system_instruction::transfer(
                            script_tokens_key,
                            game.winner_key,
                            prize);
            script::sign_instruction(&mut to_winner, 0);
            script::process_instruction(to_winner)?;
        },
    }
}
```
