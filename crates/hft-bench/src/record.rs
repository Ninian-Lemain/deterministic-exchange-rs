//! Machine-readable result records. One JSON object per line with a stable
//! key order so outputs are commit-comparable by text diff.

/// Extra typed values attached to a record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Extra {
    U64(u64),
    Text(&'static str),
}

/// Maximum named parameters carried inline without heap allocation.
pub const PARAM_SLOTS: usize = 5;

/// One benchmark cell: identity, latency distribution, throughput, allocation
/// deltas, and a deterministic checksum of observed effects.
#[derive(Debug)]
pub struct BenchRecord {
    pub boundary: &'static str,
    pub component: &'static str,
    pub scenario: &'static str,
    pub params: [(&'static str, Extra); PARAM_SLOTS],
    pub param_count: usize,
    pub samples: usize,
    pub mean_ns: u64,
    pub p50_ns: u64,
    pub p90_ns: u64,
    pub p99_ns: u64,
    pub p99_9_ns: u64,
    pub max_ns: u64,
    pub ops_per_second: u64,
    pub allocations: u64,
    pub deallocations: u64,
    pub checksum: u64,
}

impl BenchRecord {
    /// Identity block with inline parameter storage; numeric fields default
    /// to zero and are filled in after sampling. Never heap-allocates.
    ///
    /// # Panics
    ///
    /// Panics when `params` exceeds [`PARAM_SLOTS`] entries.
    #[must_use]
    pub fn new(
        boundary: &'static str,
        component: &'static str,
        scenario: &'static str,
        params: &[(&'static str, Extra)],
    ) -> Self {
        assert!(params.len() <= PARAM_SLOTS, "param slot overflow");
        let mut storage = [("", Extra::U64(0)); PARAM_SLOTS];
        storage[..params.len()].copy_from_slice(params);
        Self {
            boundary,
            component,
            scenario,
            params: storage,
            param_count: params.len(),
            samples: 0,
            mean_ns: 0,
            p50_ns: 0,
            p90_ns: 0,
            p99_ns: 0,
            p99_9_ns: 0,
            max_ns: 0,
            ops_per_second: 0,
            allocations: 0,
            deallocations: 0,
            checksum: 0,
        }
    }

    /// Serializes one line. Keys appear in declaration order; strings emitted
    /// here are static identifiers and need no escaping.
    #[must_use]
    pub fn to_json_line(&self) -> std::string::String {
        let mut line = std::string::String::with_capacity(256);
        line.push_str("{\"schema\":\"hft-bench-results/1\"");
        push_str_field(&mut line, "boundary", self.boundary);
        push_str_field(&mut line, "component", self.component);
        push_str_field(&mut line, "scenario", self.scenario);
        if self.param_count == 0 {
            line.push_str(",\"params\":{}");
        } else {
            line.push_str(",\"params\":{");
            for (index, (name, value)) in self.params[..self.param_count].iter().enumerate() {
                if index > 0 {
                    line.push(',');
                }
                line.push('"');
                line.push_str(name);
                line.push_str("\":");
                match value {
                    Extra::U64(inner) => line.push_str(&inner.to_string()),
                    Extra::Text(inner) => {
                        line.push('"');
                        line.push_str(inner);
                        line.push('"');
                    }
                }
            }
            line.push('}');
        }
        push_u64_field(
            &mut line,
            "samples",
            u64::try_from(self.samples).unwrap_or(u64::MAX),
        );
        push_u64_field(&mut line, "mean_ns", self.mean_ns);
        push_u64_field(&mut line, "p50_ns", self.p50_ns);
        push_u64_field(&mut line, "p90_ns", self.p90_ns);
        push_u64_field(&mut line, "p99_ns", self.p99_ns);
        push_u64_field(&mut line, "p99_9_ns", self.p99_9_ns);
        push_u64_field(&mut line, "max_ns", self.max_ns);
        push_u64_field(&mut line, "ops_per_second", self.ops_per_second);
        push_u64_field(&mut line, "allocations", self.allocations);
        push_u64_field(&mut line, "deallocations", self.deallocations);
        line.push_str(",\"checksum\":\"");
        for shift in (0..64).step_by(4).rev() {
            let digit = ((self.checksum >> shift) & 0xF) as usize;
            line.push(HEX_DIGITS[digit].into());
        }
        line.push_str("\"}");
        line
    }
}

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

fn push_str_field(line: &mut std::string::String, name: &str, value: &str) {
    line.push_str(",\"");
    line.push_str(name);
    line.push_str("\":\"");
    line.push_str(value);
    line.push('"');
}

fn push_u64_field(line: &mut std::string::String, name: &str, value: u64) {
    line.push_str(",\"");
    line.push_str(name);
    line.push_str("\":");
    line.push_str(&value.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> BenchRecord {
        BenchRecord {
            boundary: "component",
            component: "book",
            scenario: "head_cancel",
            params: [
                ("depth", Extra::U64(16)),
                ("", Extra::U64(0)),
                ("", Extra::U64(0)),
                ("", Extra::U64(0)),
                ("", Extra::U64(0)),
            ],
            param_count: 1,
            samples: 2_000,
            mean_ns: 67,
            p50_ns: 100,
            p90_ns: 100,
            p99_ns: 100,
            p99_9_ns: 400,
            max_ns: 340_900,
            ops_per_second: 14_925_373,
            allocations: 0,
            deallocations: 0,
            checksum: 0x10,
        }
    }

    #[test]
    fn json_line_has_stable_key_order_and_hex_checksum() {
        let expected = concat!(
            "{\"schema\":\"hft-bench-results/1\",",
            "\"boundary\":\"component\",",
            "\"component\":\"book\",",
            "\"scenario\":\"head_cancel\",",
            "\"params\":{\"depth\":16},",
            "\"samples\":2000,",
            "\"mean_ns\":67,",
            "\"p50_ns\":100,",
            "\"p90_ns\":100,",
            "\"p99_ns\":100,",
            "\"p99_9_ns\":400,",
            "\"max_ns\":340900,",
            "\"ops_per_second\":14925373,",
            "\"allocations\":0,",
            "\"deallocations\":0,",
            "\"checksum\":\"0000000000000010\"}",
        );
        assert_eq!(sample_record().to_json_line(), expected);
    }

    #[test]
    fn empty_params_render_as_empty_object_and_text_extras_quote() {
        let mut record = sample_record();
        record.params[0] = ("shape", Extra::Text("dense"));
        let line = record.to_json_line();
        assert!(line.contains("\"params\":{\"shape\":\"dense\"}"));
        record.param_count = 0;
        assert!(record.to_json_line().contains("\"params\":{}"));
    }
}
