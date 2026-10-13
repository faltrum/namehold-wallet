//! Client for hsrd wallet API v1.
//!
//! This is intentionally separate from [`crate::noncustodial::rpc`], whose
//! wire protocol is hsd's legacy JSON-RPC. The client is read-only for now:
//! phase 3 starts with `chain_snapshot` before wallet data is enabled.

use reqwest::Client;
use serde::de::{DeserializeOwned, Error as SerdeError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::error::AppError;
use crate::noncustodial::network::Network;
use crate::noncustodial::rpc::{NodeCoin, NodeCovenant};
use crate::noncustodial::tx::blake2b256;

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct HsrdRpcClient {
    http: Client,
    endpoint: String,
    authorization_header: Option<String>,
}

#[derive(Debug, Serialize)]
struct WalletRequest<'a> {
    api_version: u32,
    request_id: String,
    call: WalletCall<'a>,
}

#[derive(Debug, Serialize)]
struct WalletCall<'a> {
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct WalletResponse<T> {
    result: Option<T>,
    error: Option<Value>,
}

/// Exact chain binding returned by hsrd for wallet reads.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ChainTip {
    pub hash: String,
    pub height: u64,
    pub median_time_past: u64,
    pub tree_root: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ChainSnapshot {
    pub chain_epoch: u64,
    pub tip: Option<ChainTip>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HsrdNameOwner {
    pub txid: String,
    pub index: u32,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HsrdNameState {
    pub name_hash: String,
    pub name_hex: String,
    pub height: u64,
    pub renewal: u64,
    pub owner: Option<HsrdNameOwner>,
    pub value: u64,
    pub highest: u64,
    pub data_hex: String,
    pub transfer: u64,
    pub revoked: u64,
    pub claimed: u64,
    pub renewals: u64,
    pub registered: bool,
    pub expired: bool,
    pub weak: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct NameEvidence {
    pub projection_version: u32,
    pub chain_epoch: u64,
    pub tip: Option<ChainTip>,
    pub current_state_hex: String,
    pub current_state: Option<HsrdNameState>,
    pub proof_state: Option<HsrdNameState>,
    pub proof_hex: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ActiveNameOwnerCoin {
    pub projection_version: u32,
    pub chain_epoch: u64,
    pub tip: ChainTip,
    pub current_state_hex: String,
    pub current_state: HsrdNameState,
    pub owner_coin: HsrdCoin,
    pub inclusion: Value,
    pub source_binding: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ConfirmedScriptsPage {
    pub chain_epoch: u64,
    pub tip: Option<ChainTip>,
    pub history: Vec<ConfirmedScriptHistory>,
    pub utxos: Vec<ConfirmedScriptUtxo>,
    pub script_examinations: usize,
    pub continuation: Option<Value>,
}

/// One wallet-index history row. The node deliberately keeps this row small;
/// the transaction body is fetched separately with `raw_transaction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedScriptHistory {
    pub script_index: usize,
    pub entry: ConfirmedScriptHistoryEntry,
    pub block_time: Option<u64>,
}

impl<'de> Deserialize<'de> for ConfirmedScriptHistory {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawHistory {
            script_index: Option<usize>,
            entry: Option<ConfirmedScriptHistoryEntry>,
            txid: Option<String>,
            block_hash: Option<String>,
            height: Option<u32>,
            transaction_position: Option<u32>,
            direction: Option<Value>,
            block_time: Option<u64>,
        }

        let raw = RawHistory::deserialize(deserializer)?;
        let script_index = raw.script_index.unwrap_or_default();
        if let Some(entry) = raw.entry {
            return Ok(Self {
                script_index,
                entry,
                block_time: raw.block_time,
            });
        }

        let direction = match raw.direction {
            Some(Value::Object(object)) => ConfirmedScriptHistoryDirection {
                received: object
                    .get("received")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                spent: object
                    .get("spent")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
            Some(Value::String(value)) => ConfirmedScriptHistoryDirection {
                received: value == "received" || value == "received+spent",
                spent: value == "spent" || value == "received+spent",
            },
            _ => ConfirmedScriptHistoryDirection {
                received: false,
                spent: false,
            },
        };
        Ok(Self {
            script_index,
            entry: ConfirmedScriptHistoryEntry {
                txid: raw.txid.ok_or_else(|| D::Error::missing_field("txid"))?,
                block_hash: raw
                    .block_hash
                    .ok_or_else(|| D::Error::missing_field("block_hash"))?,
                height: raw
                    .height
                    .ok_or_else(|| D::Error::missing_field("height"))?,
                transaction_position: raw
                    .transaction_position
                    .ok_or_else(|| D::Error::missing_field("transaction_position"))?,
                direction,
            },
            block_time: raw.block_time,
        })
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ConfirmedScriptHistoryEntry {
    pub txid: String,
    pub block_hash: String,
    pub height: u32,
    pub transaction_position: u32,
    pub direction: ConfirmedScriptHistoryDirection,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ConfirmedScriptHistoryDirection {
    pub received: bool,
    pub spent: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RawTransaction {
    pub chain_epoch: u64,
    pub tip: Option<ChainTip>,
    pub txid: String,
    pub raw_hex: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfirmedScriptsScan {
    pub history: Vec<ConfirmedScriptHistory>,
    pub utxos: Vec<ConfirmedScriptUtxo>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ConfirmedScriptUtxo {
    pub script_index: usize,
    pub coin: HsrdCoin,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HsrdCoin {
    pub outpoint: HsrdOutpoint,
    pub value: u64,
    pub height: u32,
    pub coinbase: bool,
    pub address: HsrdAddress,
    pub covenant: HsrdCovenant,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HsrdOutpoint {
    pub txid: String,
    pub index: u32,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HsrdAddress {
    pub version: u8,
    pub hash: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HsrdCovenant {
    pub kind: u8,
    pub items: Vec<String>,
}

impl HsrdCoin {
    pub fn to_node_coin(&self, network: Network) -> Result<NodeCoin, AppError> {
        if self.address.version != 0 || self.address.hash.len() != 40 {
            return Err(AppError::InvalidInput(
                "hsrd returned an unsupported output address".to_string(),
            ));
        }
        let address_hash = hex::decode(&self.address.hash).map_err(|_| {
            AppError::InvalidInput("hsrd returned a non-hex output address hash".to_string())
        })?;
        let mut hash160 = [0u8; 20];
        hash160.copy_from_slice(&address_hash);
        let address = crate::noncustodial::address::encode_p2wpkh(network, &hash160)?;
        let script = format!("0014{}", self.address.hash.to_ascii_lowercase());

        Ok(NodeCoin {
            txid: self.outpoint.txid.clone(),
            vout: self.outpoint.index,
            value: i64::try_from(self.value).map_err(|_| {
                AppError::InvalidInput("hsrd returned an out-of-range coin value".to_string())
            })?,
            script: Some(script),
            address: Some(address),
            height: Some(i64::from(self.height)),
            confirmations: None,
            coinbase: Some(self.coinbase),
            covenant: Some(NodeCovenant {
                kind: self.covenant.kind,
                action: None,
                items: self.covenant.items.clone(),
            }),
        })
    }
}

pub fn script_id_from_p2wpkh_script(script_pubkey_hex: &str) -> Result<String, AppError> {
    let script = hex::decode(script_pubkey_hex)
        .map_err(|_| AppError::InvalidInput("invalid wallet script hex".to_string()))?;
    if script.len() != 22 || script[0] != 0 || script[1] != 20 {
        return Err(AppError::InvalidInput(
            "hsrd script_id requires a canonical P2WPKH script".to_string(),
        ));
    }
    // hns-node-rs serializes Address as: witness version, hash length,
    // witness-program bytes. The length byte is part of the canonical
    // identity; hashing only version + hash produces a different ScriptId.
    let mut canonical_address = Vec::with_capacity(22);
    canonical_address.push(0);
    canonical_address.push(20);
    canonical_address.extend_from_slice(&script[2..]);
    Ok(hex::encode(blake2b256(&canonical_address)))
}

impl HsrdRpcClient {
    /// Build a client for an hsrd wallet endpoint.
    ///
    /// `authorization_header` is the complete value expected after the
    /// `Authorization` header name (for example `Bearer ...`), not a token
    /// that this client will reinterpret.
    pub fn new(endpoint: &str, authorization_header: Option<&str>) -> Result<Self, AppError> {
        let endpoint = endpoint.trim_end_matches('/').to_string();
        if endpoint.is_empty() {
            return Err(AppError::InvalidInput(
                "hsrd wallet API URL must not be empty".to_string(),
            ));
        }
        let parsed = url::Url::parse(&endpoint)
            .map_err(|e| AppError::InvalidInput(format!("invalid hsrd wallet API URL: {e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(AppError::InvalidInput(
                "hsrd wallet API URL must use http or https".to_string(),
            ));
        }

        let authorization_header = authorization_header
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        Ok(Self {
            http: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(AppError::Http)?,
            endpoint,
            authorization_header,
        })
    }

    pub fn from_settings(
        settings: &std::collections::HashMap<String, String>,
    ) -> Result<Self, AppError> {
        let base = settings
            .get("node_rpc_url")
            .map(String::as_str)
            .unwrap_or("http://127.0.0.1:12037");
        let endpoint = if base.ends_with("/api/v1/wallet") {
            base.to_string()
        } else {
            format!("{base}/api/v1/wallet")
        };
        Self::new(
            &endpoint,
            settings
                .get("hsrd_authorization_header")
                .map(String::as_str),
        )
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub async fn chain_snapshot(&self) -> Result<ChainSnapshot, AppError> {
        self.call("chain_snapshot", None).await
    }

    /// Read the canonical current NameState and active owner coin under one
    /// immutable chain snapshot.
    pub async fn active_name_owner_coin(
        &self,
        name_hash: &str,
        expected_chain_epoch: u64,
    ) -> Result<ActiveNameOwnerCoin, AppError> {
        validate_hash_hex(name_hash, "hsrd name_hash")?;
        let result: ActiveNameOwnerCoin = self
            .call(
                "active_name_owner_coin",
                Some(json!({
                    "name_hash": name_hash,
                    "expected_chain_epoch": expected_chain_epoch,
                })),
            )
            .await?;
        if result.chain_epoch != expected_chain_epoch {
            return Err(AppError::Rpc(
                "hsrd active name owner response changed chain epoch".to_string(),
            ));
        }
        if !result
            .current_state
            .name_hash
            .eq_ignore_ascii_case(name_hash)
        {
            return Err(AppError::Rpc(
                "hsrd active name owner response returned a different name hash".to_string(),
            ));
        }
        if result
            .current_state
            .owner
            .as_ref()
            .map(|owner| (&owner.txid, owner.index))
            != Some((
                &result.owner_coin.outpoint.txid,
                result.owner_coin.outpoint.index,
            ))
        {
            return Err(AppError::Rpc(
                "hsrd active name owner response has inconsistent owner evidence".to_string(),
            ));
        }
        Ok(result)
    }

    /// Read proof-backed name evidence under one immutable chain snapshot.
    pub async fn name_evidence(
        &self,
        name_hash: &str,
        expected_chain_epoch: u64,
    ) -> Result<NameEvidence, AppError> {
        validate_hash_hex(name_hash, "hsrd name_hash")?;
        self.call(
            "name_evidence",
            Some(json!({
                "name_hash": name_hash,
                "expected_chain_epoch": expected_chain_epoch,
            })),
        )
        .await
    }

    /// Fetch a retained transaction body under the same chain snapshot used
    /// by the confirmed wallet scan. `raw_hex == None` is a valid pruned
    /// result and must not be mistaken for an empty transaction.
    pub async fn raw_transaction(
        &self,
        txid: &str,
        expected_chain_epoch: u64,
    ) -> Result<RawTransaction, AppError> {
        validate_hash_hex(txid, "hsrd txid")?;
        let result: RawTransaction = self
            .call(
                "raw_transaction",
                Some(json!({
                    "txid": txid,
                    "expected_chain_epoch": expected_chain_epoch,
                })),
            )
            .await?;
        if result.chain_epoch != expected_chain_epoch {
            return Err(AppError::Rpc(
                "hsrd raw transaction response changed chain epoch".to_string(),
            ));
        }
        if !result.txid.eq_ignore_ascii_case(txid) {
            return Err(AppError::Rpc(
                "hsrd raw transaction response returned a different txid".to_string(),
            ));
        }
        Ok(result)
    }

    /// Fetch one bounded page of confirmed history and UTXOs.
    ///
    /// The cursor is deliberately kept opaque: hsrd owns its encoding and
    /// clients must return it unchanged when requesting the next page.
    pub async fn confirmed_scripts_page(
        &self,
        script_ids: &[String],
        cursor: Option<Value>,
        limit: usize,
    ) -> Result<ConfirmedScriptsPage, AppError> {
        validate_script_ids(script_ids)?;
        if !(1..=256).contains(&limit) {
            return Err(AppError::InvalidInput(
                "hsrd confirmed_scripts_page limit must be between 1 and 256".to_string(),
            ));
        }
        self.call(
            "confirmed_scripts_page",
            Some(json!({
                "script_ids": script_ids,
                "cursor": cursor,
                "limit": limit,
            })),
        )
        .await
    }

    /// Consume every confirmed page under one chain snapshot.
    ///
    /// A changed epoch discards all partial results and returns an explicit
    /// error so the caller can restart from a fresh snapshot.
    pub async fn confirmed_scripts_all(
        &self,
        script_ids: &[String],
        snapshot: &ChainSnapshot,
    ) -> Result<ConfirmedScriptsScan, AppError> {
        let mut cursor = None;
        let mut result = ConfirmedScriptsScan::default();
        loop {
            let page = self.confirmed_scripts_page(script_ids, cursor, 256).await?;
            if page.chain_epoch != snapshot.chain_epoch {
                return Err(AppError::Rpc(
                    "hsrd chain changed during confirmed wallet scan; restart from a new snapshot"
                        .to_string(),
                ));
            }
            result.history.extend(page.history);
            result.utxos.extend(page.utxos);
            cursor = page.continuation;
            if cursor.is_none() {
                return Ok(result);
            }
        }
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<T, AppError> {
        let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed).to_string();
        let request = WalletRequest {
            api_version: 1,
            request_id: request_id.clone(),
            call: WalletCall { method, params },
        };

        let mut builder = self.http.post(&self.endpoint).json(&request);
        if let Some(value) = &self.authorization_header {
            builder = builder.header(reqwest::header::AUTHORIZATION, value);
        }

        let response = builder.send().await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(AppError::Rpc(format!(
                "hsrd wallet API returned HTTP {status}: {}",
                truncate_body(&body)
            )));
        }

        let envelope: WalletResponse<T> = serde_json::from_str(&body).map_err(|error| {
            AppError::Rpc(format!("hsrd wallet API returned invalid JSON: {error}"))
        })?;
        if let Some(error) = envelope.error {
            return Err(AppError::Rpc(format!("hsrd wallet API error: {error}")));
        }
        envelope.result.ok_or_else(|| {
            AppError::Rpc(format!(
                "hsrd wallet API response for request {request_id} contained no result"
            ))
        })
    }
}

fn truncate_body(body: &str) -> String {
    const MAX_BODY: usize = 512;
    let body = body.trim();
    if body.len() <= MAX_BODY {
        body.to_string()
    } else {
        format!("{}…", &body[..MAX_BODY])
    }
}

fn validate_script_ids(script_ids: &[String]) -> Result<(), AppError> {
    if script_ids.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(AppError::InvalidInput(
            "hsrd script_ids must be sorted and unique".to_string(),
        ));
    }
    if script_ids
        .iter()
        .any(|id| id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(AppError::InvalidInput(
            "hsrd script_ids must be 32-byte hexadecimal strings".to_string(),
        ));
    }
    Ok(())
}

fn validate_hash_hex(value: &str, label: &str) -> Result<(), AppError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::InvalidInput(format!(
            "{label} must be a 32-byte hexadecimal string"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::{Matcher, Server};
    use reqwest::StatusCode;

    #[tokio::test]
    async fn chain_snapshot_sends_v1_envelope_and_authorization() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/wallet")
            .match_header("authorization", "Bearer test")
            .match_body(Matcher::Regex(
                r#""api_version":1.*"method":"chain_snapshot""#.to_string(),
            ))
            .with_status(200)
            .with_body(
                r#"{"result":{"chain_epoch":42,"tip":{"hash":"abc","height":7,"median_time_past":123,"tree_root":"def"}}}"#,
            )
            .create_async()
            .await;

        let client = HsrdRpcClient::new(
            &format!("{}/api/v1/wallet", server.url()),
            Some("Bearer test"),
        )
        .unwrap();
        assert_eq!(
            client.chain_snapshot().await.unwrap(),
            ChainSnapshot {
                chain_epoch: 42,
                tip: Some(ChainTip {
                    hash: "abc".to_string(),
                    height: 7,
                    median_time_past: 123,
                    tree_root: "def".to_string(),
                }),
            }
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn confirmed_scripts_page_preserves_opaque_cursor() {
        let mut server = Server::new_async().await;
        let script_id = "00".repeat(32);
        let mock = server
            .mock("POST", "/api/v1/wallet")
            .match_body(Matcher::Regex(
                r#""method":"confirmed_scripts_page""#.to_string(),
            ))
            .with_status(200)
            .with_body(
                r#"{"result":{"chain_epoch":42,"tip":null,"history":[],"utxos":[],"script_examinations":1,"continuation":null}}"#,
            )
            .create_async()
            .await;
        let client = HsrdRpcClient::new(&format!("{}/api/v1/wallet", server.url()), None).unwrap();
        let page = client
            .confirmed_scripts_page(&[script_id], Some(Value::String("next".to_string())), 2)
            .await
            .unwrap();
        assert_eq!(page.chain_epoch, 42);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn active_name_owner_coin_binds_epoch_and_owner_outpoint() {
        let mut server = Server::new_async().await;
        let hash = "11".repeat(32);
        let txid = "22".repeat(32);
        let mock = server
            .mock("POST", "/api/v1/wallet")
            .match_body(Matcher::Regex(
                r#""method":"active_name_owner_coin".*"expected_chain_epoch":42"#.to_string(),
            ))
            .with_status(200)
            .with_body(format!(
                r#"{{"result":{{"projection_version":1,"chain_epoch":42,"tip":{{"hash":"aa","height":100,"median_time_past":1,"tree_root":"bb"}},"current_state_hex":"00","current_state":{{"name_hash":"{hash}","name_hex":"616263","height":10,"renewal":20,"owner":{{"txid":"{txid}","index":0}},"value":1,"highest":1,"data_hex":"","transfer":0,"revoked":0,"claimed":0,"renewals":0,"registered":true,"expired":false,"weak":false}},"owner_coin":{{"outpoint":{{"txid":"{txid}","index":0}},"value":1,"height":10,"coinbase":false,"address":{{"version":0,"hash":"{hash20}"}},"covenant":{{"kind":6,"items":[]}}}},"inclusion":{{}},"source_binding":"trusted_node_active_utxo_projection"}}}}"#,
                hash20 = "33".repeat(20)
            ))
            .create_async()
            .await;
        let client = HsrdRpcClient::new(&format!("{}/api/v1/wallet", server.url()), None).unwrap();
        let result = client.active_name_owner_coin(&hash, 42).await.unwrap();
        assert_eq!(result.current_state.name_hex, "616263");
        assert_eq!(result.owner_coin.outpoint.txid, txid);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn confirmed_scripts_all_collects_pages_and_rejects_epoch_changes() {
        let mut server = Server::new_async().await;
        let script_id = "11".repeat(32);
        let first = server
            .mock("POST", "/api/v1/wallet")
            .with_status(200)
            .with_body(format!(
                r#"{{"result":{{"chain_epoch":7,"tip":null,"history":[],"utxos":[{{"script_index":0,"coin":{{"outpoint":{{"txid":"{}","index":1}},"value":12,"height":5,"coinbase":false,"address":{{"version":0,"hash":"{}"}},"covenant":{{"kind":0,"items":[]}}}}}}],"script_examinations":1,"continuation":"next"}}}}"#,
                "aa".repeat(32),
                "bb".repeat(20)
            ))
            .create_async()
            .await;
        let second = server
            .mock("POST", "/api/v1/wallet")
            .with_status(200)
            .with_body(
                r#"{"result":{"chain_epoch":7,"tip":null,"history":[],"utxos":[],"script_examinations":1,"continuation":null}}"#,
            )
            .create_async()
            .await;
        let client = HsrdRpcClient::new(&format!("{}/api/v1/wallet", server.url()), None).unwrap();
        let rows = client
            .confirmed_scripts_all(
                &[script_id],
                &ChainSnapshot {
                    chain_epoch: 7,
                    tip: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(rows.utxos.len(), 1);
        assert_eq!(rows.utxos[0].coin.value, 12);
        first.assert_async().await;
        second.assert_async().await;
    }

    #[test]
    fn rejects_invalid_script_sets_and_limits() {
        let duplicate = "00".repeat(32);
        assert!(validate_script_ids(&[duplicate.clone(), duplicate]).is_err());
        assert!(validate_script_ids(&["not-hex".to_string()]).is_err());
    }

    #[test]
    fn derives_script_id_and_converts_hsrd_coin() {
        let script_id =
            script_id_from_p2wpkh_script("00140000000000000000000000000000000000000000").unwrap();
        assert_eq!(script_id.len(), 64);

        let coin = HsrdCoin {
            outpoint: HsrdOutpoint {
                txid: "aa".repeat(32),
                index: 1,
            },
            value: 42,
            height: 7,
            coinbase: false,
            address: HsrdAddress {
                version: 0,
                hash: "11".repeat(20),
            },
            covenant: HsrdCovenant {
                kind: 0,
                items: vec![],
            },
        };
        let node_coin = coin.to_node_coin(Network::Main).unwrap();
        assert_eq!(node_coin.txid, "aa".repeat(32));
        assert_eq!(node_coin.vout, 1);
        assert_eq!(node_coin.value, 42);
        assert_eq!(node_coin.height, Some(7));
        assert!(node_coin.address.unwrap().starts_with("hs1"));
    }

    #[test]
    fn accepts_flat_and_nested_history_rows() {
        let flat: ConfirmedScriptHistory = serde_json::from_value(json!({
            "txid": "aa".repeat(32),
            "block_hash": "bb".repeat(32),
            "height": 7,
            "transaction_position": 2,
            "direction": {"received": true, "spent": false}
        }))
        .unwrap();
        assert!(flat.entry.direction.received);
        let nested: ConfirmedScriptHistory = serde_json::from_value(json!({
            "script_index": 1,
            "entry": {
                "txid": "cc".repeat(32),
                "block_hash": "dd".repeat(32),
                "height": 8,
                "transaction_position": 3,
                "direction": {"received": false, "spent": true}
            },
            "block_time": 123
        }))
        .unwrap();
        assert_eq!(nested.script_index, 1);
        assert!(nested.entry.direction.spent);
    }

    #[tokio::test]
    async fn reports_http_and_malformed_response_errors() {
        let mut server = Server::new_async().await;
        let unauthorized = server
            .mock("POST", "/api/v1/wallet")
            .with_status(StatusCode::UNAUTHORIZED.as_u16() as usize)
            .with_body("denied")
            .create_async()
            .await;
        let client = HsrdRpcClient::new(&format!("{}/api/v1/wallet", server.url()), None).unwrap();
        let error = client.chain_snapshot().await.unwrap_err();
        assert!(error.to_string().contains("HTTP 401"));
        unauthorized.assert_async().await;

        let malformed = server
            .mock("POST", "/api/v1/wallet")
            .with_status(200)
            .with_body("not-json")
            .create_async()
            .await;
        let error = client.chain_snapshot().await.unwrap_err();
        assert!(error.to_string().contains("invalid JSON"));
        malformed.assert_async().await;
    }
}
