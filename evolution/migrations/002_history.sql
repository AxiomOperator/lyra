-- The evolution history is append-only, enforced: no row is ever changed
-- or removed. Events also say what kind of change they concern and the
-- runs that were its evidence.
ALTER TABLE evolution_events ADD COLUMN category TEXT;
ALTER TABLE evolution_events ADD COLUMN evidence TEXT NOT NULL DEFAULT '[]';

CREATE TRIGGER evolution_events_append_only_update BEFORE UPDATE ON evolution_events
BEGIN
    SELECT RAISE(ABORT, 'the evolution history is append-only');
END;

CREATE TRIGGER evolution_events_append_only_delete BEFORE DELETE ON evolution_events
BEGIN
    SELECT RAISE(ABORT, 'the evolution history is append-only');
END;
