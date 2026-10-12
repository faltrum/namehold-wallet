# HSRD integration

<!--
This document describes the current experimental integration boundary. Keep the
setup instructions aligned with the node lifecycle and wallet API code before
changing the supported `hsrd` version or endpoint contract.
-->

Namehold can connect to **hsrd**, the native Rust Handshake full node from
`handshake-rs/hns-node-rs`. The integration is deliberately incremental:
`hsd` remains the default engine and the existing `hsd` JSON-RPC/REST protocol
is not mixed with the `hsrd` wallet API.

## What is supported today

The current adapter supports the read-only wallet reconciliation path:

1. Select `hsrd` as the node engine in **Settings → Connections**.
2. Start or adopt one local `hsrd` process.
3. Authenticate against `POST /api/v1/wallet`.
4. Capture a `chain_snapshot`.
5. Convert each derived P2WPKH output into the canonical `hsrd` script ID.
6. Read paginated confirmed UTXOs with `confirmed_scripts_page`.
7. Convert the response into Namehold's `NodeCoin` model.
8. Persist the result in `tracked_utxos` and refresh the wallet balance.

The adapter retries a scan when `hsrd` reports `stale_snapshot` or when the
chain epoch changes during pagination. A retry discards partial results rather
than applying a mixed-chain UTXO set.

## Current limitations

The following surfaces are intentionally not enabled for `hsrd` yet:

- name discovery and name evidence;
- name actions such as renew, reveal, redeem, register, update, and transfer;
- transaction construction against `hsrd`-specific evidence;
- transaction broadcast;
- Shakedex purchases;
- background synchronization through the legacy `hsd` path.

As a result, a successful `hsrd wallet sync` means that UTXOs were reconciled.
It does not mean that names or sending are ready. Namehold's existing `hsd`
engine continues to provide the complete legacy workflow.

## Requirements

Use an `hsrd` build that includes the v1 wallet API described by the
`hns-node-rs` wallet RPC documentation. The node must be started with:

```text
--wallet-index
--rpc-authorization-header-file=/path/to/authorization-file
```

The wallet API is not installed without the authorization-file option. In that
case, `/api/v1/status` and `/api/v1/sync` may still respond while
`/api/v1/wallet` returns `404 Not Found`.

The node and Namehold must use the same data directory and network. Only one
`hsrd` process may own a data directory at a time.

## Configure Namehold

Open **Settings → Connections** and choose `hsrd`.

Use these values for a local mainnet node:

| Setting | Value |
|---|---|
| Node engine | `hsrd` |
| Node RPC URL | `http://127.0.0.1:12037` |
| Node RPC API key | Leave empty |
| hsrd Authorization header | A complete header value, for example `Bearer namehold-hsrd-local-2026` |
| hsrd binary path | The path to `hsrd`, or empty for auto-detection |

The authorization field contains only the value after the HTTP header name.
Enter:

```text
Bearer namehold-hsrd-local-2026
```

Do not enter:

```text
Authorization: Bearer namehold-hsrd-local-2026
```

Namehold stores the authorization value as a protected secret and writes the
private authorization file when it starts `hsrd`. The value is intentionally
redacted when Settings is reopened; a stored-secret marker is shown instead.
Leaving the field blank while saving other settings preserves the stored value.

## Start `hsrd` from Namehold

This is the recommended workflow because Namehold creates the authorization
file and passes the correct flags:

1. Stop any existing `hsrd` process using the same data directory.
2. Start Namehold with `pnpm tauri dev`.
3. Select `hsrd` and save the connection settings.
4. Start the node from the Namehold node controls.
5. Wait for the node status to become reachable.
6. Press **Sync**.

On mainnet, the process should include arguments equivalent to:

```text
hsrd \
  --network mainnet \
  --data-dir /path/to/mainnet-wallet \
  --rpc-bind 127.0.0.1:12037 \
  --wallet-index \
  --rpc-authorization-header-file /path/to/namehold-hsrd.authorization
```

Namehold translates its internal `main` network name to hsrd's required
`mainnet` CLI value. `testnet`, `regtest`, and `simnet` are passed through.

## Start `hsrd` manually

Manual startup is also supported, but the authorization file must be created
before the node starts:

```bash
AUTH_FILE="$PWD/data/mainnet-wallet/namehold-hsrd.authorization"
printf '%s\n' 'Bearer namehold-hsrd-local-2026' > "$AUTH_FILE"
chmod 600 "$AUTH_FILE"

./target/release/hsrd \
  --network mainnet \
  --data-dir "$PWD/data/mainnet-wallet" \
  --rpc-bind 127.0.0.1:12037 \
  --wallet-index \
  --rpc-authorization-header-file "$AUTH_FILE"
```

When `hsrd` is started manually, do not press **Start node** in Namehold.
Namehold should connect to the already-running process instead of trying to
spawn a second one.

## Verify the API

Check the chain snapshot with the same authorization value:

```bash
curl -i \
  -X POST http://127.0.0.1:12037/api/v1/wallet \
  -H 'Authorization: Bearer namehold-hsrd-local-2026' \
  -H 'Content-Type: application/json' \
  --data '{"api_version":1,"request_id":"manual-test","call":{"method":"chain_snapshot"}}'
```

A successful response contains `200 OK`, `chain_epoch`, and `tip`. The
`chain_snapshot` call must not include an empty `params` object. Methods with
arguments, such as `confirmed_scripts_page`, do include `params`.

The diagnostic endpoints are separate:

```bash
curl -sS http://127.0.0.1:12037/api/v1/status
curl -sS http://127.0.0.1:12037/api/v1/sync
```

The **Test connection** button uses these diagnostic endpoints for `hsrd`; it
does not send the legacy `hsd` `getblockchaininfo` request.

## Troubleshooting

### `HTTP 404 Not Found` from `/api/v1/wallet`

The running process was probably started without:

```text
--rpc-authorization-header-file
```

Stop that process and restart it with the authorization file. A node started
without this option can still expose status and sync diagnostics while hiding
the wallet route.

### `HTTP 400 Bad Request`

Check the request envelope. `chain_snapshot` must be sent as:

```json
{
  "api_version": 1,
  "request_id": "example",
  "call": {
    "method": "chain_snapshot"
  }
}
```

### `HTTP 409` with `stale_snapshot`

The chain or mempool changed between the snapshot and the page scan. This is
expected while the node is receiving blocks. Namehold retries this operation
and applies no partial result.

### `Resource temporarily unavailable` or `chain/LOCK`

Two `hsrd` processes are trying to use the same data directory. Check the
process and port before starting another node:

```bash
pgrep -a hsrd
ss -ltnp | grep 12037
```

Use one of these approaches, not both:

- start `hsrd` from Namehold; or
- start it manually and let Namehold adopt it.

### Sync completes but the spendable balance is zero

The wallet separates liquid HNS from name-controlled value. A successful
scan may report UTXOs with a name covenant, which appear as **Name Value**
rather than **Spendable**. The local cache can be inspected by checking the
`tracked_utxos` table in `~/.namehold/portfolio.db`.

The current integration logs the scan size and result:

```text
hsrd wallet sync: scanning 40 derived output scripts
hsrd wallet sync: received 2 UTXOs
```

If the count is zero, verify that the wallet profile's derived addresses are
the addresses that actually received the funds and that the `hsrd` wallet
index was built for the same chain.

### Names do not appear after an `hsrd` sync

This is currently expected. Namehold has not yet connected the `hsrd` name
evidence methods to the existing name cache. Use `hsd` for the complete names
workflow until that phase is implemented.

## Data and security notes

Namehold remains non-custodial:

- private keys and recovery material stay on the device;
- `hsrd` supplies chain state and indexed UTXOs;
- the authorization header authenticates local API requests;
- the authorization value is stored as a sensitive setting and is not logged;
- the authorization file is created with restrictive permissions on Unix;
- the node data directory must not be shared by multiple node processes.

The `hsrd` adapter is read-only with respect to transaction broadcast. Enabling
`hsrd` does not silently change the default engine or enable sending.

## Development boundary

The main implementation surfaces are:

- `src-tauri/src/noncustodial/hsrd_rpc.rs`: wallet API v1 client, pagination,
  epoch validation, script IDs, and coin conversion;
- `src-tauri/src/commands/node.rs`: process lifecycle, authorization file,
  network argument mapping, and diagnostics;
- `src-tauri/src/commands/sync.rs`: UTXO reconciliation and retry handling;
- `src-tauri/src/sql/034_node_engine.sql`: persistent engine and hsrd settings.

<!--
When the name-evidence and broadcast phases land, update the supported matrix
above and the release-note entry together. Do not describe hsrd as fully
supported until names and transaction safety checks have their own tests.
-->
