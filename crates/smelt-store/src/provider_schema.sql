CREATE TABLE acp_provider_thread (
  session_id TEXT NOT NULL REFERENCES acp_detail(session_id) ON DELETE CASCADE,
  provider_key TEXT NOT NULL,
  agent TEXT NOT NULL,
  profile_id TEXT,
  agent_definition_id TEXT,
  history_session_id TEXT,
  covered_segment INTEGER NOT NULL CHECK (covered_segment >= 0),
  PRIMARY KEY (session_id, provider_key)
) WITHOUT ROWID;
CREATE TABLE acp_provider_segment (
  session_id TEXT NOT NULL,
  position INTEGER NOT NULL CHECK (position >= 0),
  provider_key TEXT NOT NULL,
  history_session_id TEXT NOT NULL,
  first_turn INTEGER NOT NULL CHECK (first_turn >= 0),
  end_turn INTEGER NOT NULL CHECK (end_turn > first_turn),
  PRIMARY KEY (session_id, position),
  FOREIGN KEY (session_id, provider_key)
    REFERENCES acp_provider_thread(session_id, provider_key) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE TABLE acp_context_handoff (
  session_id TEXT NOT NULL,
  delivery_id TEXT NOT NULL,
  provider_key TEXT NOT NULL,
  history_session_id TEXT,
  through_segment INTEGER NOT NULL CHECK (through_segment >= 0),
  status TEXT NOT NULL CHECK (status IN ('prepared', 'delivered', 'unknown')),
  PRIMARY KEY (session_id, delivery_id),
  FOREIGN KEY (session_id, provider_key)
    REFERENCES acp_provider_thread(session_id, provider_key) ON DELETE CASCADE
) WITHOUT ROWID;
