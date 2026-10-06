use crate::{
    CheckpointError, Engine, EngineConfig, EngineError, Health, InstrumentId, RouteTable, ShardId,
};
use hft_io::RxFrame;
use hft_recovery::Snapshot;
use hft_types::Command;
use hft_wire::{ParseError, parse_message};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoutedEngineBuildError {
    InstrumentMismatch {
        shard: ShardId,
        route: InstrumentId,
        engine: InstrumentId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoutedEngineError {
    Parse(ParseError),
    UnknownInstrument(InstrumentId),
    Engine { shard: ShardId, error: EngineError },
}

#[derive(Debug)]
pub enum RoutedCheckpointError {
    UnknownShard(ShardId),
    Engine(CheckpointError),
}

/// Fixed routes over independent journaled engines. Arrays are indexed by shard.
/// Commands apply synchronously; event and journal consumers remain independent.
/// No mutable engine or gateway access is exposed.
pub struct RoutedEngine<
    'storage,
    const SHARDS: usize,
    const ACCOUNTS: usize,
    const RISK_ORDERS: usize,
    const LEVELS: usize,
    const ORDERS: usize,
    const REPORTS: usize,
    const BATCH: usize,
    const EVENTS: usize,
> {
    routes: RouteTable<SHARDS>,
    engines:
        [Engine<'storage, ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS, REPORTS, BATCH, EVENTS>; SHARDS],
}

impl<
    'storage,
    const SHARDS: usize,
    const ACCOUNTS: usize,
    const RISK_ORDERS: usize,
    const LEVELS: usize,
    const ORDERS: usize,
    const REPORTS: usize,
    const BATCH: usize,
    const EVENTS: usize,
> RoutedEngine<'storage, SHARDS, ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS, REPORTS, BATCH, EVENTS>
{
    /// # Errors
    ///
    /// Rejects an engine whose instrument differs from its configured shard.
    pub fn try_new(
        routes: RouteTable<SHARDS>,
        engines: [Engine<'storage, ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS, REPORTS, BATCH, EVENTS>;
            SHARDS],
    ) -> Result<Self, RoutedEngineBuildError> {
        for route in routes.routes() {
            let engine = &engines[usize::from(route.shard_id.0)];
            if engine.instrument() != route.instrument_id {
                return Err(RoutedEngineBuildError::InstrumentMismatch {
                    shard: route.shard_id,
                    route: route.instrument_id,
                    engine: engine.instrument(),
                });
            }
        }
        Ok(Self { routes, engines })
    }

    #[must_use]
    pub const fn routes(&self) -> &RouteTable<SHARDS> {
        &self.routes
    }

    /// # Errors
    ///
    /// Parse and unknown-route errors consume nothing. Engine errors preserve
    /// the single-instrument engine contract, including unchanged overload retries.
    pub fn process_frame(&mut self, frame: &RxFrame<'_>) -> Result<ShardId, RoutedEngineError> {
        let command = parse_message(frame)
            .map_err(RoutedEngineError::Parse)?
            .to_command();
        self.process_command(command)
    }

    /// Routes through the engine's sequence, risk, event, and journal admission.
    /// Successful application includes business rejections and consumes the sequence.
    ///
    /// # Errors
    ///
    /// Unknown routes consume nothing. Backpressure leaves both the command and
    /// its sequence unconsumed. Other errors retain the engine's fatality contract.
    pub fn process_command(&mut self, command: Command) -> Result<ShardId, RoutedEngineError> {
        let shard = self.routes.shard_for(command.instrument_id()).ok_or(
            RoutedEngineError::UnknownInstrument(command.instrument_id()),
        )?;
        self.engines[usize::from(shard.0)]
            .process_command(command)
            .map_err(|error| RoutedEngineError::Engine { shard, error })?;
        Ok(shard)
    }

    #[must_use]
    pub fn health(&self, shard: ShardId) -> Option<Health> {
        self.engines.get(usize::from(shard.0)).map(Engine::health)
    }

    #[must_use]
    pub fn configuration(&self, shard: ShardId) -> Option<&EngineConfig> {
        self.engines
            .get(usize::from(shard.0))
            .map(Engine::configuration)
    }

    /// Closes every journal producer. Workers must still drain and flush.
    pub fn stop_admission(&mut self) {
        for engine in &mut self.engines {
            engine.stop_admission();
        }
    }

    /// # Errors
    ///
    /// Rejects unknown shards or any engine checkpoint refusal.
    pub fn snapshot(&self, shard: ShardId) -> Result<Snapshot, RoutedCheckpointError> {
        self.engines
            .get(usize::from(shard.0))
            .ok_or(RoutedCheckpointError::UnknownShard(shard))?
            .snapshot()
            .map_err(RoutedCheckpointError::Engine)
    }
}
