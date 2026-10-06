use hft_risk::RiskLimits;
use hft_types::{AccountId, InstrumentId, PriceTicks, Quantity};
use std::fmt::Write;

/// Canonical replay sidecar. Queue sizes are operational, not replay semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineConfig {
    pub instrument: InstrumentId,
    /// Account, reservation, price level, orders per level, and report capacities.
    pub capacities: [usize; 5],
    pub accounts: Vec<(AccountId, RiskLimits)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigError {
    Encoding,
    UnsupportedVersion,
    NonCanonical,
    InvalidCapacity,
    InvalidAccounts,
    Mismatch,
}

impl EngineConfig {
    /// # Errors
    /// Rejects invalid capacity shapes, account definitions, and report bounds.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.capacities.contains(&0) || self.capacities[4] > 65_534 {
            return Err(ConfigError::InvalidCapacity);
        }
        if self.accounts.len() > self.capacities[0]
            || self.accounts.windows(2).any(|pair| pair[0].0 >= pair[1].0)
            || self.accounts.iter().any(|(_, limits)| {
                limits.max_quantity.0 == 0
                    || limits.max_open_orders == 0
                    || limits.minimum_price > limits.maximum_price
            })
        {
            return Err(ConfigError::InvalidAccounts);
        }
        Ok(())
    }

    /// Version 1 is UTF-8 decimal fields with a final newline and ascending accounts.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut text = format!(
            "HFTCONFIG 1 {} {} {}\n{} {} {} {} {} {}\n",
            hft_wire::PROTOCOL_VERSION,
            hft_journal::FORMAT_VERSION,
            hft_recovery::FORMAT_VERSION,
            self.instrument.0,
            self.capacities[0],
            self.capacities[1],
            self.capacities[2],
            self.capacities[3],
            self.capacities[4]
        );
        for (id, limits) in &self.accounts {
            // Writing to a String is infallible.
            let _ = writeln!(
                text,
                "{} {} {} {} {} {} {}",
                id.0,
                limits.max_quantity.0,
                limits.max_notional,
                limits.max_abs_position.0,
                limits.max_open_orders,
                limits.minimum_price.0,
                limits.maximum_price.0
            );
        }
        text.into_bytes()
    }

    /// # Errors
    /// Rejects unknown formats, malformed fields, invalid limits and noncanonical bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, ConfigError> {
        let text = std::str::from_utf8(bytes).map_err(|_| ConfigError::Encoding)?;
        let mut lines = text.lines();
        let header = lines.next().ok_or(ConfigError::Encoding)?;
        let expected = format!(
            "HFTCONFIG 1 {} {} {}",
            hft_wire::PROTOCOL_VERSION,
            hft_journal::FORMAT_VERSION,
            hft_recovery::FORMAT_VERSION
        );
        if header != expected {
            return Err(ConfigError::UnsupportedVersion);
        }
        let mut fields = lines
            .next()
            .ok_or(ConfigError::Encoding)?
            .split_whitespace();
        let instrument = InstrumentId(field(&mut fields)?);
        let capacities = [
            field(&mut fields)?,
            field(&mut fields)?,
            field(&mut fields)?,
            field(&mut fields)?,
            field(&mut fields)?,
        ];
        if fields.next().is_some() {
            return Err(ConfigError::Encoding);
        }
        let mut accounts = Vec::new();
        for line in lines {
            let mut fields = line.split_whitespace();
            let id = AccountId(field(&mut fields)?);
            let limits = RiskLimits {
                max_quantity: Quantity(field(&mut fields)?),
                max_notional: field(&mut fields)?,
                max_abs_position: Quantity(field(&mut fields)?),
                max_open_orders: field(&mut fields)?,
                minimum_price: PriceTicks(field(&mut fields)?),
                maximum_price: PriceTicks(field(&mut fields)?),
            };
            if fields.next().is_some() {
                return Err(ConfigError::Encoding);
            }
            accounts.push((id, limits));
        }
        let config = Self {
            instrument,
            capacities,
            accounts,
        };
        config.validate()?;
        if config.encode() != bytes {
            return Err(ConfigError::NonCanonical);
        }
        Ok(config)
    }

    /// Upgrade and rollback both require identical replay semantics and supported formats.
    /// # Errors
    /// Rejects changes to instrument, risk definitions, capacities, or report bound.
    pub fn check_compatible(&self, candidate: &Self) -> Result<(), ConfigError> {
        self.validate()?;
        candidate.validate()?;
        if self != candidate {
            return Err(ConfigError::Mismatch);
        }
        Ok(())
    }
}

fn field<T: std::str::FromStr>(
    fields: &mut std::str::SplitWhitespace<'_>,
) -> Result<T, ConfigError> {
    fields
        .next()
        .ok_or(ConfigError::Encoding)?
        .parse()
        .map_err(|_| ConfigError::Encoding)
}
