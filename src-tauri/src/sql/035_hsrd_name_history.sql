-- Historical name activity discovered from hsrd's confirmed script index.
--
-- `tracked_name_states` remains the mutable current projection. This table is
-- append/upsert evidence: it preserves bids, reveals, renewals, transfers,
-- and other covenant actions even after their owner coins are spent.

CREATE TABLE IF NOT EXISTS hsrd_name_history (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    wallet_profile_id   TEXT NOT NULL REFERENCES wallet_profiles(id) ON DELETE CASCADE,
    txid                TEXT NOT NULL,
    vout                INTEGER NOT NULL,
    name_hash_hex       TEXT NOT NULL,
    name                TEXT,
    covenant_type       INTEGER NOT NULL,
    value_doos          INTEGER NOT NULL,
    height              INTEGER,
    direction           TEXT NOT NULL,
    covenant_json       TEXT NOT NULL,
    UNIQUE(wallet_profile_id, txid, vout)
);

CREATE INDEX IF NOT EXISTS idx_hsrd_name_history_profile_name
    ON hsrd_name_history(wallet_profile_id, name);
CREATE INDEX IF NOT EXISTS idx_hsrd_name_history_profile_hash
    ON hsrd_name_history(wallet_profile_id, name_hash_hex);
