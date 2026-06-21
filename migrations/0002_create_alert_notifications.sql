-- Append-only audit log: one row per Threema notification actually sent by the scheduler.
--
-- The recipient is deliberately NOT stored here to avoid persisting PII. While the originating
-- alert exists, the recipient is recoverable by joining `alert_uid` to `alerts.threema_id`. When
-- the alert is deleted, `ON DELETE SET NULL` clears `alert_uid` (so the recipient is dropped with
-- it) while the audit row itself remains.
CREATE TABLE alert_notifications (
    uid        INTEGER PRIMARY KEY,
    -- Originating alert; SET NULL (not cascade) so the audit row outlives the alert.
    alert_uid  INTEGER REFERENCES alerts(uid) ON DELETE SET NULL,
    -- Sensor the notification was about (not PII; retained after alert deletion).
    sensor_id  INTEGER NOT NULL,
    -- Why the notification was sent. Stored as the snake_case variant name of NotificationReason.
    reason     TEXT    NOT NULL DEFAULT 'threshold_reached',
    -- The day's swim-window average that triggered the notification, in °C.
    swim_avg   REAL    NOT NULL,
    -- The alert threshold at send time, in °C.
    threshold  REAL    NOT NULL,
    -- Unix epoch seconds.
    sent_at    INTEGER NOT NULL
);
