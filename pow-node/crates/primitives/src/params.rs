use alloy::primitives::{Address, B256, Bytes, U256};
use serde::{Deserialize, Serialize};

use crate::node::Log;

/// Which block a state query refers to.
///
/// Breeja has no reorgs and no notion of finality — once a block is mined it is
/// final — so `Safe` and `Finalized` resolve to the same block as `Latest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockTag {
    Earliest,
    Latest,
    Pending,
    Safe,
    Finalized,
    Number(u64),
}

impl BlockTag {
    /// Parses a block parameter as it arrives over JSON-RPC.
    pub fn parse(raw: &str) -> Result<Self, anyhow::Error> {
        match raw {
            "earliest" => Ok(Self::Earliest),
            "latest" => Ok(Self::Latest),
            "pending" => Ok(Self::Pending),
            "safe" => Ok(Self::Safe),
            "finalized" => Ok(Self::Finalized),
            other => {
                let hex = other.strip_prefix("0x").ok_or_else(|| {
                    anyhow::anyhow!(
                        "invalid block parameter {other:?}: expected a block tag or a 0x-prefixed number"
                    )
                })?;
                let number = u64::from_str_radix(hex, 16)
                    .map_err(|e| anyhow::anyhow!("invalid block number {other:?}: {e}"))?;
                Ok(Self::Number(number))
            }
        }
    }

    /// Resolves this tag against a chain whose head is at `head`.
    ///
    /// `Pending` resolves to the head too: the pending block does not exist until
    /// the miner seals it, and Breeja answers pending account queries from the
    /// mempool instead.
    pub fn resolve(&self, head: u64) -> Result<u64, anyhow::Error> {
        match self {
            Self::Earliest => Ok(0),
            Self::Latest | Self::Pending | Self::Safe | Self::Finalized => Ok(head),
            Self::Number(number) if *number <= head => Ok(*number),
            Self::Number(number) => {
                anyhow::bail!("block {number} does not exist yet; chain head is {head}")
            }
        }
    }
}

impl<'de> Deserialize<'de> for BlockTag {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// The transaction-shaped object `eth_call` and `eth_estimateGas` take.
///
/// Every field is optional, because a caller may omit anything the node can
/// default. `data` and `input` are the same field under two names; Ethereum
/// clients accept both, so Breeja does too.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRequest {
    pub from: Option<Address>,
    pub to: Option<Address>,
    pub gas: Option<U256>,
    pub gas_price: Option<U256>,
    pub value: Option<U256>,
    pub data: Option<Bytes>,
    pub input: Option<Bytes>,
}

impl CallRequest {
    /// The calldata this request carries, under whichever name it was sent.
    pub fn calldata(&self) -> Bytes {
        self.data
            .clone()
            .or_else(|| self.input.clone())
            .unwrap_or_default()
    }
}

/// A field that accepts either a single value or a list of them, as
/// `eth_getLogs` does for `address` and for each topic position.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T: PartialEq> OneOrMany<T> {
    /// Whether `value` is allowed. An empty list allows anything, which is how
    /// geth reads `"address": []`.
    fn allows(&self, value: &T) -> bool {
        match self {
            Self::One(one) => one == value,
            Self::Many(many) => many.is_empty() || many.contains(value),
        }
    }
}

/// The filter object `eth_getLogs` takes.
///
/// Either a block range (`fromBlock`/`toBlock`, both defaulting to `latest`) or
/// a single `blockHash`, never both.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogFilter {
    pub from_block: Option<BlockTag>,
    pub to_block: Option<BlockTag>,
    pub block_hash: Option<B256>,
    pub address: Option<OneOrMany<Address>>,
    pub topics: Option<Vec<Option<OneOrMany<B256>>>>,
}

impl LogFilter {
    /// Whether `log` passes the address and topic constraints. The block
    /// constraints are applied by whoever walks the chain.
    ///
    /// Topic positions are the unintuitive part. Position `i` constrains
    /// `log.topics[i]`:
    ///
    /// - `null` matches anything at that position;
    /// - a single hash must equal it;
    /// - a list matches *any* of its hashes (OR within a position, AND across
    ///   positions).
    ///
    /// And, following geth, a log with fewer topics than the filter has
    /// positions never matches — even if the extra positions are `null`. So
    /// `[null]` means "has at least one topic", not "anything".
    pub fn matches(&self, log: &Log) -> bool {
        if let Some(address) = &self.address
            && !address.allows(&log.address)
        {
            return false;
        }

        let Some(topics) = &self.topics else {
            return true;
        };
        if topics.len() > log.topics.len() {
            return false;
        }
        topics
            .iter()
            .zip(&log.topics)
            .all(|(wanted, actual)| wanted.as_ref().is_none_or(|wanted| wanted.allows(actual)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_block_tag_ethereum_defines() {
        assert_eq!(BlockTag::parse("latest").unwrap(), BlockTag::Latest);
        assert_eq!(BlockTag::parse("earliest").unwrap(), BlockTag::Earliest);
        assert_eq!(BlockTag::parse("pending").unwrap(), BlockTag::Pending);
        assert_eq!(BlockTag::parse("safe").unwrap(), BlockTag::Safe);
        assert_eq!(BlockTag::parse("finalized").unwrap(), BlockTag::Finalized);
        assert_eq!(BlockTag::parse("0x1f").unwrap(), BlockTag::Number(31));
        assert!(BlockTag::parse("31").is_err());
        assert!(BlockTag::parse("0xzz").is_err());
    }

    #[test]
    fn resolves_tags_against_the_chain_head() {
        assert_eq!(BlockTag::Latest.resolve(9).unwrap(), 9);
        assert_eq!(BlockTag::Pending.resolve(9).unwrap(), 9);
        assert_eq!(BlockTag::Earliest.resolve(9).unwrap(), 0);
        assert_eq!(BlockTag::Number(4).resolve(9).unwrap(), 4);
        // A block that has not been mined yet is an error, not a silent default.
        assert!(BlockTag::Number(10).resolve(9).is_err());
    }

    // Replaces `a_call_needs_an_evm_when_it_carries_calldata_or_creates_a_contract`.
    // That test pinned down `needs_evm()`, which decided whether the node could
    // answer at all. With an EVM every request can be answered, so the method and
    // its test are gone; what remains to get right is reading the calldata.
    #[test]
    fn calldata_is_read_from_either_field_name() {
        let bytes = Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]);

        let as_data = CallRequest {
            data: Some(bytes.clone()),
            ..Default::default()
        };
        let as_input = CallRequest {
            input: Some(bytes.clone()),
            ..Default::default()
        };
        assert_eq!(as_data.calldata(), bytes);
        assert_eq!(as_input.calldata(), bytes);
        assert!(CallRequest::default().calldata().is_empty());

        // `to` is still optional: no recipient means a deployment.
        let deployment: CallRequest = serde_json::from_str(r#"{"data":"0x6000"}"#).unwrap();
        assert_eq!(deployment.to, None);
        assert_eq!(deployment.calldata(), Bytes::from_static(&[0x60, 0x00]));
    }

    fn log(address: Address, topics: &[B256]) -> Log {
        Log {
            address,
            topics: topics.to_vec(),
            ..Default::default()
        }
    }

    fn filter(json: &str) -> LogFilter {
        serde_json::from_str(json).expect("a valid filter")
    }

    #[test]
    fn a_log_filter_parses_every_shape_the_spec_allows() {
        let empty = filter("{}");
        assert!(empty.from_block.is_none() && empty.address.is_none() && empty.topics.is_none());

        let full = filter(&format!(
            r#"{{"fromBlock":"0x1","toBlock":"latest","address":["{a}","{a}"],
                 "topics":[null,"{t}",["{t}","{t}"]]}}"#,
            a = Address::repeat_byte(0x01),
            t = B256::repeat_byte(0xaa),
        ));
        assert_eq!(full.from_block, Some(BlockTag::Number(1)));
        assert_eq!(full.to_block, Some(BlockTag::Latest));
        assert!(matches!(full.address, Some(OneOrMany::Many(ref list)) if list.len() == 2));
        let topics = full.topics.unwrap();
        assert_eq!(topics[0], None);
        assert_eq!(topics[1], Some(OneOrMany::One(B256::repeat_byte(0xaa))));
        assert!(matches!(topics[2], Some(OneOrMany::Many(_))));
    }

    #[test]
    fn a_log_filter_matches_by_address() {
        let (a, b) = (Address::repeat_byte(0x01), Address::repeat_byte(0x02));
        let one = filter(&format!(r#"{{"address":"{a}"}}"#));
        assert!(one.matches(&log(a, &[])));
        assert!(!one.matches(&log(b, &[])));

        let either = filter(&format!(r#"{{"address":["{a}","{b}"]}}"#));
        assert!(either.matches(&log(a, &[])) && either.matches(&log(b, &[])));

        // No address constraint at all, or an empty list, matches everything.
        assert!(filter("{}").matches(&log(b, &[])));
        assert!(filter(r#"{"address":[]}"#).matches(&log(b, &[])));
    }

    #[test]
    fn topic_positions_combine_wildcards_and_alternatives() {
        let any = Address::repeat_byte(0x01);
        let (x, y, z) = (
            B256::repeat_byte(0x0a),
            B256::repeat_byte(0x0b),
            B256::repeat_byte(0x0c),
        );

        // Exact match at position 0.
        let exact = filter(&format!(r#"{{"topics":["{x}"]}}"#));
        assert!(exact.matches(&log(any, &[x])));
        assert!(exact.matches(&log(any, &[x, y])), "extra log topics are fine");
        assert!(!exact.matches(&log(any, &[y])));

        // A wildcard first position, then X or Y second.
        let second = filter(&format!(r#"{{"topics":[null,["{x}","{y}"]]}}"#));
        assert!(second.matches(&log(any, &[z, x])));
        assert!(second.matches(&log(any, &[z, y])));
        assert!(!second.matches(&log(any, &[z, z])));

        // A filter with more positions than the log has topics never matches,
        // even when those positions are wildcards.
        assert!(!filter("{\"topics\":[null]}").matches(&log(any, &[])));
        assert!(filter("{\"topics\":[null]}").matches(&log(any, &[z])));
        assert!(!second.matches(&log(any, &[x])));

        // An empty topics list constrains nothing.
        assert!(filter("{\"topics\":[]}").matches(&log(any, &[])));
    }
}
