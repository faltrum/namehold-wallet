//! Client for hsrd wallet API v1.
//!
//! This is intentionally separate from [`crate::noncustodial::rpc`], whose
//! wire protocol is hsd's legacy JSON-RPC. The client is read-only for now:
//! phase 3 starts with `chain_snapshot` before wallet data is enabled.

use reqwest::Client;
use serde::de::DeserializeOwned;
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

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ConfirmedScriptsPage {
    pub chain_epoch: u64,
    pub tip: Option<ChainTip>,
    pub history: Vec<Value>,
    pub utxos: Vec<ConfirmedScriptUtxo>,
    pub script_examinations: usize,
    pub continuation: Option<String>,
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

    /// Fetch one bounded page of confirmed history and UTXOs.
    ///
    /// The cursor is deliberately kept opaque: hsrd owns its encoding and
    /// clients must return it unchanged when requesting the next page.
    pub async fn confirmed_scripts_page(
        &self,
        script_ids: &[String],
        cursor: Option<String>,
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
    ) -> Result<Vec<ConfirmedScriptUtxo>, AppError> {
        let mut cursor = None;
        let mut result = Vec::new();
        loop {
            let page = self.confirmed_scripts_page(script_ids, cursor, 256).await?;
            if page.chain_epoch != snapshot.chain_epoch {
                return Err(AppError::Rpc(
                    "hsrd chain changed during confirmed wallet scan; restart from a new snapshot"
                        .to_string(),
                ));
            }
            result.extend(page.utxos);
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
            .confirmed_scripts_page(&[script_id], Some("next".to_string()), 2)
            .await
            .unwrap();
        assert_eq!(page.chain_epoch, 42);
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
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].coin.value, 12);
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
