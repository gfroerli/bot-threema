use chrono_tz::Tz;

pub mod alert;
pub mod api;
pub mod chart;
pub mod config;
pub mod db;
pub mod handler;
pub mod store;

/// Timezone all sensor data and user-facing times are expressed in.
///
/// Sensors are Swiss water bodies; incoming UTC data is converted into this zone for both chart
/// display and the alert swim-window evaluation, so the two share one definition.
pub const LOCAL_TIMEZONE: Tz = Tz::Europe__Zurich;
