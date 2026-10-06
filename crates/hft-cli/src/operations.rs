use hft_engine::{
    EngineBuilder, EngineConfig, EngineParts, EngineStorage, FlushPolicy, PersistenceWorker,
    RecoveryBundle, RiskLimits,
};
use hft_types::{
    AccountId, Command, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, SequenceNumber,
    Side, TimeInForce,
};
use std::{fmt::Debug, path::Path};

fn describe(error: impl Debug) -> String {
    format!("{error:?}")
}

pub fn run(command: &str, args: &[String]) -> Result<(), String> {
    match (command, args) {
        ("config-validate", [path]) => {
            let config = read_config(path)?;
            println!("valid instrument={} capacities={:?} accounts={}", config.instrument.0, config.capacities, config.accounts.len());
        }
        ("compatibility-check", [current, candidate]) => {
            read_config(current)?.check_compatible(&read_config(candidate)?).map_err(describe)?;
            println!("compatible for upgrade or rollback; replay formats and configuration unchanged");
        }
        ("restore-check" | "health", [directory, expected]) => {
            let bundle = RecoveryBundle::read(Path::new(directory)).map_err(describe)?;
            let builder = bundle.restore::<2, 8, 4, 4, 4>(&read_config(expected)?).map_err(describe)?;
            let mut storage = EngineStorage::<6, 4>::try_new().map_err(describe)?;
            let parts = builder.build(&mut storage).map_err(describe)?;
            println!("recoverable instrument={} next_sequence={} (offline; no service running)", parts.engine.instrument().0, parts.engine.expected_sequence().0);
        }
        ("backup", [source, destination, expected]) => {
            let bundle = RecoveryBundle::read(Path::new(source)).map_err(describe)?;
            bundle.restore::<2, 8, 4, 4, 4>(&read_config(expected)?).map_err(describe)?;
            bundle.backup_new::<2, 8, 4, 4, 4>(Path::new(destination)).map_err(describe)?;
            println!("backup published at {destination}");
        }
        ("checkpoint-demo", [directory]) => checkpoint(Path::new(directory))?,
        _ => return Err("usage: hft-cli config-validate CONFIG | compatibility-check CURRENT CANDIDATE | checkpoint-demo NEW_DIRECTORY | health BUNDLE EXPECTED_CONFIG | restore-check BUNDLE EXPECTED_CONFIG | backup BUNDLE NEW_DIRECTORY EXPECTED_CONFIG (recovery binary capacities: 2,8,4,4,4)".to_owned()),
    }
    Ok(())
}

fn read_config(path: &str) -> Result<EngineConfig, String> {
    EngineConfig::decode(&std::fs::read(path).map_err(describe)?).map_err(describe)
}

fn checkpoint(destination: &Path) -> Result<(), String> {
    let limits = RiskLimits {
        max_quantity: Quantity(100),
        max_notional: 100_000,
        max_abs_position: Quantity(1_000),
        max_open_orders: 8,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(1_000),
    };
    let mut storage = EngineStorage::<6, 4>::try_new().map_err(describe)?;
    let EngineParts {
        mut engine,
        mut events,
        journal,
    } = EngineBuilder::<2, 8, 4, 4, 4>::new(
        InstrumentId(1),
        &[(AccountId(1), limits), (AccountId(2), limits)],
    )
    .map_err(describe)?
    .build(&mut storage)
    .map_err(describe)?;
    // Real callers keep a sink worker running while admitting.
    let mut worker = PersistenceWorker::<_, 8>::new(
        journal,
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination.with_extension("journal"))
            .map_err(describe)?,
        FlushPolicy::OnShutdown,
    )
    .map_err(describe)?;
    engine
        .process_command(Command::NewOrder(NewOrder {
            order_id: OrderId(1),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(1),
            price: PriceTicks(100),
            quantity: Quantity(5),
            side: Side::Buy,
            time_in_force: TimeInForce::Gtc,
        }))
        .map_err(describe)?;
    engine.stop_admission();
    worker.shutdown().map_err(describe)?;
    while events.try_pop().is_some() {}
    let bundle = RecoveryBundle::checkpoint(
        engine.configuration(),
        &engine.snapshot().map_err(describe)?,
    );
    bundle.publish_new(destination).map_err(describe)?;
    println!(
        "checkpoint published at {} after admission closure, journal flush and event drain",
        destination.display()
    );
    Ok(())
}
