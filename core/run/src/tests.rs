use std::{
    convert::AsRef,
    env::{current_dir, set_current_dir},
    fs, io,
    path::{Path, PathBuf},
    str::FromStr,
};

use clap::{builder::TypedValueParser as _, Command};

use common_config_parser::types::{
    spec::{ChainSpec, ChainSpecValueParser, PrivateKeyFileValueParser},
    Config, ConfigValueParser,
};
use common_crypto::Secp256k1RecoverablePrivateKey;
use protocol::{
    codec::hex_decode,
    tokio,
    types::{RichBlock, H256},
};

use crate::{components::chain_spec::ChainSpecExt as _, execute_transactions, DatabaseGroup};

const DEV_CONFIG_DIR: &str = "../../devtools/chain";

struct TestCase<'a> {
    chain_name:            &'a str,
    config_file:           &'a str,
    chain_spec_file:       &'a str,
    key_file:              &'a str,
    input_genesis_hash:    &'a str,
    genesis_state_root:    &'a str,
    genesis_receipts_root: &'a str,
}

const TESTCASES: &[TestCase] = &[
    TestCase {
        chain_name:            "single_node",
        config_file:           "config.toml",
        chain_spec_file:       "specs/single_node/chain-spec.toml",
        key_file:              "debug.key",
        input_genesis_hash:    "0x4e06dc4a01178db42c029f7d65f65a5763702a21082cfcb626c6c41054a7a276",
        genesis_state_root:    "0xb6c51706f77a788606c8b932ad4f752c3f9a54008cd82e85f417d3a7e900d5c2",
        genesis_receipts_root: "0x7e747618f612d08dfe54bcb67f58f13a49e8b1bafee9a8f19a3a0f7122f44d02",
    },
    TestCase {
        chain_name:            "multi_nodes",
        config_file:           "nodes/node_1.toml",
        chain_spec_file:       "specs/multi_nodes/chain-spec.toml",
        key_file:              "debug.key",
        input_genesis_hash:    "0xf16db25ca1a0cff5339d76e9802c75c43faac35ee4a9294a51234b167c69159f",
        genesis_state_root:    "0x9f76f02e823115e7b9887d07678f40b8ff50f82d4d09214fa481bdad68f5192a",
        genesis_receipts_root: "0x7e747618f612d08dfe54bcb67f58f13a49e8b1bafee9a8f19a3a0f7122f44d02",
    },
    TestCase {
        chain_name:            "multi_nodes_short_epoch_len",
        config_file:           "nodes/node_1.toml",
        chain_spec_file:       "specs/multi_nodes_short_epoch_len/chain-spec.toml",
        key_file:              "debug.key",
        input_genesis_hash:    "0x4e06dc4a01178db42c029f7d65f65a5763702a21082cfcb626c6c41054a7a276",
        genesis_state_root:    "0x9dddf9df077b2bb98f72686256dae92fb975d416fa3093956151056b913a065d",
        genesis_receipts_root: "0x7e747618f612d08dfe54bcb67f58f13a49e8b1bafee9a8f19a3a0f7122f44d02",
    },
];

#[test]
fn decode_genesis() {
    let raw = fs::read("../../tests/data/genesis.json").unwrap();
    assert!(serde_json::from_slice::<RichBlock>(&raw).is_ok());
}

#[test]
fn decode_type_id() {
    let type_id_str = "c0810210210c06ba233273e94d7fc89b00a705a07fdc0ae4c78e4036582ff336";
    assert!(hex_decode(type_id_str).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn genesis_data_for_dev_chain() {
    for case in TESTCASES {
        check_genesis_data(case).await;
    }
}

async fn check_genesis_data<'a>(case: &TestCase<'a>) {
    let dev_config_dir = PathBuf::from_str(DEV_CONFIG_DIR).expect("read dev config dir");
    let tmp_dir = tempfile::tempdir().unwrap_or_else(|err| {
        panic!("failed to create temporary directory since {err:?}");
    });
    let tmp_dir_path = tmp_dir.path();
    let command = Command::new("dummy-command");

    copy_dir(dev_config_dir, tmp_dir_path).expect("configs copied");
    let current_dir = current_dir().expect("get current directory");
    set_current_dir(tmp_dir_path).expect("change work directory");

    let config: Config = {
        let config_path = tmp_dir_path.join(case.config_file);
        ConfigValueParser
            .parse_ref(&command, None, config_path.as_os_str())
            .expect("parse config file")
    };
    let chain_spec: ChainSpec = {
        let chain_spec_path = tmp_dir_path.join(case.chain_spec_file);
        ChainSpecValueParser
            .parse_ref(&command, None, chain_spec_path.as_os_str())
            .expect("parse chain-spec file")
    };
    let key: Secp256k1RecoverablePrivateKey = {
        let key_file_path = tmp_dir_path.join(case.key_file);
        let key_data = PrivateKeyFileValueParser
            .parse_ref(&command, None, key_file_path.as_os_str())
            .expect("parse key file");
        Secp256k1RecoverablePrivateKey::try_from(key_data.as_ref()).expect("load key data")
    };
    let genesis = chain_spec.generate_genesis_block(key);

    check_hashes(
        case.chain_name,
        "input genesis hash",
        case.input_genesis_hash,
        genesis.block.header.hash(),
    );

    for (i, (block_cached, tx)) in genesis
        .block
        .tx_hashes
        .iter()
        .zip(genesis.txs.iter())
        .enumerate()
    {
        let tx_cached = tx.transaction.hash;
        assert_eq!(
            *block_cached, tx_cached,
            "check hash of tx[{i}], in-block: {block_cached:#x}, tx-cached: {tx_cached:#x}",
        );
        let calculated = tx.transaction.clone().calc_hash().hash;
        assert_eq!(
            tx_cached, calculated,
            "check hash of tx[{i}], cached: {tx_cached:#x}, calculated: {calculated:#x}",
        );
    }
    let path_block = tmp_dir.path().join("block");
    let db_group = DatabaseGroup::new(
        &config.rocksdb,
        path_block,
        true,
        config.executor.triedb_cache_size,
    )
    .expect("initialize databases");

    let resp = execute_transactions(&genesis, &db_group, &chain_spec.accounts)
        .expect("execute transactions");

    check_hashes(
        case.chain_name,
        "genesis state root",
        case.genesis_state_root,
        resp.state_root,
    );
    check_hashes(
        case.chain_name,
        "genesis receipts root",
        case.genesis_receipts_root,
        resp.receipt_root,
    );

    set_current_dir(current_dir).expect("change back to original work directory");
}

fn check_hashes(chain: &str, name: &str, expected_str: &str, actual: H256) {
    let expected = H256::from_str(expected_str)
        .unwrap_or_else(|err| panic!("failed to parse hash {name} of chain {chain} since {err}"));
    assert_eq!(
        expected, actual,
        "hash {name} of chain {chain} is changed, expect {expected:#x}, but got {actual:#x}",
    );
}

fn copy_dir(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> io::Result<()> {
    fs::create_dir_all(&dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir(entry.path(), dst.as_ref().join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.as_ref().join(entry.file_name()))?;
        }
    }
    Ok(())
}
