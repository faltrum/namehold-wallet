-- Selects the local node implementation used by the managed node lifecycle.
-- hsd remains the default; hsrd support is opt-in while its wallet adapter is
-- introduced incrementally.
INSERT OR IGNORE INTO settings (key, value) VALUES
    ('node_engine', 'hsd'),
    ('hsrd_path', ''),
    ('hsrd_authorization_header', '');
