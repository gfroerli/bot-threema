-- Swimming-temperature alerts.
--
-- One row per (user, sensor) pair. Each row also carries the alert state machine: an alert is
-- `watching` until its sensor's afternoon average crosses the threshold on two consecutive days, at
-- which point it `notifies` the user and becomes `notified`; it resets to `watching` after the
-- average stays clearly below the threshold for three consecutive days.
CREATE TABLE alerts (
    uid            INTEGER PRIMARY KEY,
    threema_id     TEXT    NOT NULL CHECK (length(threema_id) = 8),
    sensor_id      INTEGER NOT NULL,
    -- Range mirrors MIN_THRESHOLD / MAX_THRESHOLD in src/store.rs; keep in sync.
    threshold      REAL    NOT NULL CHECK (threshold >= 5 AND threshold <= 40),
    status         TEXT    NOT NULL DEFAULT 'watching',
    warm_streak    INTEGER NOT NULL DEFAULT 0,
    cold_streak    INTEGER NOT NULL DEFAULT 0,
    -- Calendar date (YYYY-MM-DD) of the last evaluation; NULL until first run.
    last_eval_date TEXT,
    -- Unix epoch seconds.
    created_at     INTEGER NOT NULL,
    UNIQUE (threema_id, sensor_id)
);
