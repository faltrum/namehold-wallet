-- Durable checkpoints for the hsrd wallet-index scan.
--
-- The continuation token is opaque and must be replayed with the same profile,
-- script set, and chain epoch that produced it. Page data and the next token
-- are written in one transaction so a crash cannot advance the token without
-- also saving the page it represents.

CREATE TABLE IF NOT EXISTS hsrd_sync_checkpoints (
    wallet_profile_id TEXT PRIMARY KEY REFERENCES wallet_profiles(id) ON DELETE CASCADE,
    chain_epoch       INTEGER NOT NULL, -- hsrd snapshot epoch bound to the scan
    script_fingerprint TEXT NOT NULL,   -- sorted/deduplicated script IDs
    continuation_json TEXT,             -- hsrd-owned opaque continuation token
    updated_at        TEXT NOT NULL DEFAULT (datetime('now'))
);

-- UTXOs collected by completed pages. These are staging rows, not the
-- wallet's authoritative tracked_utxos projection.
CREATE TABLE IF NOT EXISTS hsrd_sync_checkpoint_utxos (
    wallet_profile_id TEXT NOT NULL REFERENCES wallet_profiles(id) ON DELETE CASCADE,
    txid              TEXT NOT NULL,
    vout              INTEGER NOT NULL,
    script_index      INTEGER NOT NULL,
    value_doos        INTEGER NOT NULL,
    height            INTEGER NOT NULL,
    coinbase          INTEGER NOT NULL,
    address_version   INTEGER NOT NULL,
    address_hash      TEXT NOT NULL,
    covenant_kind     INTEGER NOT NULL,
    covenant_items_json TEXT NOT NULL,
    PRIMARY KEY (wallet_profile_id, txid, vout)
);

-- History collected by completed pages. The final sync consumes these rows
-- only after the continuation token reaches the end of the result set.
CREATE TABLE IF NOT EXISTS hsrd_sync_checkpoint_history (
    wallet_profile_id TEXT NOT NULL REFERENCES wallet_profiles(id) ON DELETE CASCADE,
    script_index      INTEGER NOT NULL,
    txid              TEXT NOT NULL,
    block_hash        TEXT NOT NULL,
    height            INTEGER NOT NULL,
    transaction_position INTEGER NOT NULL,
    received          INTEGER NOT NULL,
    spent             INTEGER NOT NULL,
    block_time        INTEGER,
    PRIMARY KEY (wallet_profile_id, txid, transaction_position)
);

CREATE INDEX IF NOT EXISTS idx_hsrd_checkpoint_utxos_profile
    ON hsrd_sync_checkpoint_utxos(wallet_profile_id);

CREATE INDEX IF NOT EXISTS idx_hsrd_checkpoint_history_profile
    ON hsrd_sync_checkpoint_history(wallet_profile_id);
