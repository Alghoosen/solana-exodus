use {
    assert_cmd::prelude::*,
    solana_ledger::{
        create_new_tmp_ledger, create_new_tmp_ledger_fifo, genesis_utils::create_genesis_config, get_tmp_ledger_path_auto_delete
    },
    std::process::{Command, Output},
};

fn run_ledger_tool(args: &[&str]) -> Output {
    Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .args(args)
        .output()
        .unwrap()
}

fn count_newlines(chars: &[u8]) -> usize {
    bytecount::count(chars, b'\n')
}

#[test]
fn bad_arguments() {
    // At least a ledger path is required
    assert!(!run_ledger_tool(&[]).status.success());

    // Invalid ledger path should fail
    assert!(!run_ledger_tool(&["-l", "invalid_ledger", "verify"])
        .status
        .success());
}

fn nominal_test_helper(
    ledger_path: &str,
    ticks: usize,
    use_default_shred_compaction: bool,
    compatible_shred_compaction: &str,
    incompatible_shred_compaction: &str,
) {
    let meta_lines = 2;
    let summary_lines = 1;

    // Basic validation
    if use_default_shred_compaction {
        let output = run_ledger_tool(&["-l", ledger_path, "verify"]);
        assert!(output.status.success());
    }

    // Repeat by manually specifying rocksdb-shred-compaction
    let output = run_ledger_tool(&[
        "-l",
        ledger_path,
        "--rocksdb-shred-compaction",
        compatible_shred_compaction,
        "verify",
    ]);
    assert!(output.status.success());

    // Repeat with an incompatible shred compaction setting and expect failure
    let output = run_ledger_tool(&[
        "-l",
        ledger_path,
        "--rocksdb-shred-compaction",
        incompatible_shred_compaction,
        "verify",
    ]);
    assert!(!output.status.success());

    // Print everything
    if use_default_shred_compaction {
        let output = run_ledger_tool(&["-l", ledger_path, "print", "-vvv"]);
        assert!(output.status.success());
        assert!(count_newlines(&output.stdout) >= meta_lines + summary_lines);
        assert_eq!(
            count_newlines(&output.stdout).saturating_sub(meta_lines + summary_lines),
            ticks
        );
    }
    let output = run_ledger_tool(&[
        "-l",
        ledger_path,
        "--rocksdb-shred-compaction",
        compatible_shred_compaction,
        "print",
        "-vvv",
    ]);
    assert!(output.status.success());
    assert!(count_newlines(&output.stdout) >= meta_lines + summary_lines);
    assert_eq!(
        count_newlines(&output.stdout).saturating_sub(meta_lines + summary_lines),
        ticks
    );
}

#[test]
fn nominal_default() {
    let genesis_config = create_genesis_config(100).genesis_config;
    let (ledger_path, _blockhash) = create_new_tmp_ledger!(&genesis_config);
    nominal_test_helper(
        ledger_path.to_str().unwrap(),
        genesis_config.ticks_per_slot as usize,
        true, // use_default_shred_compaction
        "level",
        "fifo",
    );
}

#[test]
fn nominal_fifo() {
    let genesis_config = create_genesis_config(100).genesis_config;
    let (ledger_path, _blockhash) = create_new_tmp_ledger_fifo!(&genesis_config);
    nominal_test_helper(
        ledger_path.to_str().unwrap(),
        genesis_config.ticks_per_slot as usize,
        false, // use_default_shred_compaction
        "fifo",
        "level",
    );
}

fn copy_test_helper(src_shred_compaction: &str, dst_shred_compaction: &str) {
    let genesis_config = create_genesis_config(100).genesis_config;
    let (ledger_path, _blockhash) = match src_shred_compaction {
        "fifo" => create_new_tmp_ledger_fifo!(&genesis_config),
        _ => create_new_tmp_ledger!(&genesis_config),
    };
    let ledger_path = ledger_path.to_str().unwrap();
    let target_ledger_path = get_tmp_ledger_path_auto_delete!();
    let target_ledger_path = target_ledger_path.path().to_str().unwrap();
    let output = run_ledger_tool(&[
        "-l",
        ledger_path,
        "--rocksdb-shred-compaction",
        src_shred_compaction,
        "copy",
        "--target-db",
        target_ledger_path,
        "--target-rocksdb-shred-compaction",
        dst_shred_compaction,
        "--ending-slot",
        "1",
    ]);
    assert!(output.status.success());
    let src_slot_output = run_ledger_tool(&[
        "-l",
        ledger_path,
        "--rocksdb-shred-compaction",
        src_shred_compaction,
        "slot",
        "0",
    ]);

    let dst_slot_output = run_ledger_tool(&[
        "-l",
        target_ledger_path,
        "--rocksdb-shred-compaction",
        dst_shred_compaction,
        "slot",
        "0",
    ]);
    assert!(src_slot_output.status.success());
    assert!(dst_slot_output.status.success());
    assert!(!src_slot_output.stdout.is_empty());
    assert_eq!(src_slot_output.stdout, dst_slot_output.stdout);
}

#[test]
fn copy_test() {
    copy_test_helper("level", "level");
    copy_test_helper("level", "fifo");
    copy_test_helper("fifo", "level");
    copy_test_helper("fifo", "fifo");
}
