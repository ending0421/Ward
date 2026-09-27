//! The JSON envelope shared by BOTH surfaces (issue #4).
//!
//! MCP tools and `ward <cmd> --json` emit the same shape:
//! `{ "ok": bool, "error"?: string, "data"?: T }`. A consumer written
//! against either surface can parse the other without silently reading an
//! empty set (`.data.matches // []` on a payload-shaped response is the
//! silently-permissive failure class a gate tool must never produce).

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Envelope<T: Serialize> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
}

impl<T: Serialize> Envelope<T> {
    pub fn ok(data: T) -> Self {
        Self {
            ok: true,
            error: None,
            data: Some(data),
        }
    }

    pub fn err(message: impl std::fmt::Display) -> Self {
        Self {
            ok: false,
            error: Some(message.to_string()),
            data: None,
        }
    }

    /// A REFUSAL (issue #17): the tool ran, the payload is attached (it
    /// explains itself via `index_state` / `cannot_answer`), but it is not an
    /// answer — `ok: false` so a consumer that only reads stdout cannot read
    /// a cannot-answer as "checked, found nothing". Distinct from [`err`],
    /// which means "no result at all".
    pub fn refused(data: T, reason: impl std::fmt::Display) -> Self {
        Self {
            ok: false,
            error: Some(reason.to_string()),
            data: Some(data),
        }
    }

    pub fn to_string_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_envelope_carries_data_without_error_field() {
        let json = Envelope::ok(42).to_string_pretty().unwrap();
        assert!(json.contains("\"ok\": true"));
        assert!(json.contains("\"data\": 42"));
        assert!(!json.contains("error"));
    }

    #[test]
    fn refused_envelope_carries_both_error_and_data() {
        // Issue #17: ok:false AND data — the payload is the explanation.
        let json = Envelope::refused(7, "cannot answer: no index")
            .to_string_pretty()
            .unwrap();
        assert!(json.contains("\"ok\": false"));
        assert!(json.contains("\"error\": \"cannot answer: no index\""));
        assert!(json.contains("\"data\": 7"), "{json}");
    }

    #[test]
    fn err_envelope_carries_error_without_data_field() {
        let json = Envelope::<()>::err("boom").to_string_pretty().unwrap();
        assert!(json.contains("\"ok\": false"));
        assert!(json.contains("\"error\": \"boom\""));
        assert!(!json.contains("\"data\""));
    }
}
