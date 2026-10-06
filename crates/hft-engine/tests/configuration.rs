use hft_engine::{
    BuildError, ConfigError, EngineBuilder, EngineConfig, RecoveryBundle, RiskLimits,
};
use hft_gateway::Gateway;
use hft_recovery::encode_snapshot;
use hft_risk::RiskEngine;
use hft_types::{AccountId, InstrumentId, PriceTicks, Quantity};

fn limits() -> RiskLimits {
    RiskLimits {
        max_quantity: Quantity(100),
        max_notional: 10_000,
        max_abs_position: Quantity(1_000),
        max_open_orders: 8,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(100),
    }
}

fn config() -> EngineConfig {
    EngineBuilder::<2, 8, 4, 4, 4>::new(
        InstrumentId(1),
        &[(AccountId(2), limits()), (AccountId(1), limits())],
    )
    .expect("builder")
    .configuration()
    .clone()
}

fn snapshot() -> hft_recovery::Snapshot {
    let mut risk = RiskEngine::<2, 8>::new();
    risk.register_account(AccountId(1), limits())
        .expect("account");
    risk.register_account(AccountId(2), limits())
        .expect("account");
    encode_snapshot(&Gateway::<2, 8, 4, 4>::new(risk, InstrumentId(1)), 0).expect("snapshot")
}

#[test]
fn canonical_config_rejects_versions_whitespace_duplicate_accounts_and_limits() {
    let original = config();
    assert_eq!(
        EngineConfig::decode(&original.encode()).expect("decode"),
        original
    );
    let mut bytes = original.encode();
    bytes.pop();
    assert_eq!(EngineConfig::decode(&bytes), Err(ConfigError::NonCanonical));
    let bytes = String::from_utf8(original.encode())
        .expect("utf8")
        .replace("HFTCONFIG 1", "HFTCONFIG 2");
    assert_eq!(
        EngineConfig::decode(bytes.as_bytes()),
        Err(ConfigError::UnsupportedVersion)
    );
    let mut invalid = original.clone();
    invalid.accounts[1].0 = invalid.accounts[0].0;
    assert_eq!(invalid.validate(), Err(ConfigError::InvalidAccounts));
    let mut invalid = original;
    invalid.accounts[0].1.max_quantity = Quantity(0);
    assert_eq!(invalid.validate(), Err(ConfigError::InvalidAccounts));
}

#[test]
fn configured_restore_fails_closed_for_report_or_account_changes() {
    let original = config();
    let snapshot = snapshot();
    EngineBuilder::<2, 8, 4, 4, 4>::restore_configured(
        &original,
        &original.encode(),
        snapshot.bytes(),
        &[],
    )
    .expect("restore");
    let mut changed = original.clone();
    changed.capacities[4] = 3;
    assert!(matches!(
        EngineBuilder::<2, 8, 4, 4, 3>::restore_configured(
            &changed,
            &original.encode(),
            snapshot.bytes(),
            &[]
        ),
        Err(BuildError::Configuration(ConfigError::Mismatch))
    ));
    changed = original;
    changed.accounts[0].1.max_notional += 1;
    assert!(matches!(
        EngineBuilder::<2, 8, 4, 4, 4>::restore_configured(
            &changed,
            &changed.encode(),
            snapshot.bytes(),
            &[]
        ),
        Err(BuildError::Configuration(ConfigError::Mismatch))
    ));
}

#[test]
fn bundle_publication_backup_and_restore_require_complete_valid_input() {
    let root = std::env::temp_dir().join(format!(
        "hft-bundle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir(&root).expect("root");
    let path = root.join("checkpoint");
    let backup = root.join("backup");
    let configuration = config();
    let bundle = RecoveryBundle::checkpoint(&configuration, &snapshot());
    bundle.publish_new(&path).expect("publish");
    assert!(bundle.publish_new(&path).is_err());
    let recovered = RecoveryBundle::read(&path).expect("read");
    recovered
        .backup_new::<2, 8, 4, 4, 4>(&backup)
        .expect("backup");
    RecoveryBundle::read(&backup)
        .expect("backup read")
        .restore::<2, 8, 4, 4, 4>(&configuration)
        .expect("restore");
    let mut tampered = configuration.clone();
    tampered.capacities[4] = 3;
    std::fs::write(backup.join("config.v1"), tampered.encode()).expect("tamper report bound");
    assert!(matches!(
        RecoveryBundle::read(&backup),
        Err(hft_engine::BundleError::IntegrityMismatch)
    ));
    std::fs::write(backup.join("config.v1"), configuration.encode()).expect("restore config");
    std::fs::remove_file(backup.join("COMMITTED")).expect("remove marker");
    assert!(RecoveryBundle::read(&backup).is_err());
    let mut corrupt = RecoveryBundle::read(&path).expect("read");
    corrupt.snapshot[0] ^= 1;
    assert!(
        corrupt
            .backup_new::<2, 8, 4, 4, 4>(&root.join("bad"))
            .is_err()
    );
    assert!(!root.join("bad").exists());
    std::fs::remove_dir_all(root).expect("cleanup");
}
#[test]
fn configuration_preserves_registration_rules_for_zero_exposure_limits() {
    let limits = RiskLimits {
        max_notional: 0,
        max_abs_position: Quantity(0),
        ..limits()
    };
    let mut risk = RiskEngine::<2, 8>::new();
    risk.register_account(AccountId(1), limits)
        .expect("zero exposure limits are valid");
    let builder = EngineBuilder::<2, 8, 4, 4, 4>::new(InstrumentId(1), &[(AccountId(1), limits)])
        .expect("matching configuration rules");
    let decoded =
        EngineConfig::decode(&builder.configuration().encode()).expect("canonical config");
    assert_eq!(decoded.accounts[0].1, limits);
}
