//! Stand-in for `iana-time-zone`, which chrono only consults when `TZ` is unset *and* `/etc/localtime`
//! cannot be parsed. On macOS the real crate links CoreFoundation (~4.5 MB RSS in every process).
//! Without it that last-resort case falls back to UTC instead.

#[derive(Debug)]
pub struct GetTimezoneError;

impl std::fmt::Display for GetTimezoneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("time zone lookup disabled")
    }
}

impl std::error::Error for GetTimezoneError {}

pub fn get_timezone() -> Result<String, GetTimezoneError> {
    Err(GetTimezoneError)
}
