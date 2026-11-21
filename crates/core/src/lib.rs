//! Everything that doesn't touch the network: the D1 schema, mapping a catalog entry onto
//! rows, turning search parameters and CQL2 into SQL, and the STAC response shapes. It builds
//! for both native (indexer, tests) and wasm32 (worker), so the same code that writes the
//! index is the code that reads it.

pub mod api;
pub mod bbox;
pub mod cql;
pub mod load;
pub mod record;
pub mod search;
pub mod ts;

pub const MIGRATION: &str = include_str!("../migrations/0001_init.sql");

/// Stands in for the API origin inside stored documents; replaced per request.
pub const BASE: &str = "{{BASE}}";

/// An error the API answers as an RFC 9457 problem.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub status: u16,
    pub title: &'static str,
    pub detail: String,
}

pub type Result<T> = std::result::Result<T, Problem>;

impl Problem {
    pub fn bad(detail: impl Into<String>) -> Self {
        Self { status: 400, title: "Bad Request", detail: detail.into() }
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self { status: 404, title: "Not Found", detail: detail.into() }
    }

    pub fn method_not_allowed() -> Self {
        Self { status: 405, title: "Method Not Allowed", detail: "this API is read-only".into() }
    }

    /// Carries no cause: the caller learns nothing that could leak a query.
    pub fn internal() -> Self {
        Self { status: 500, title: "Internal Server Error", detail: "the request could not be completed".into() }
    }

    pub fn json(&self) -> String {
        serde_json::json!({
            "type": "about:blank",
            "title": self.title,
            "status": self.status,
            "detail": self.detail,
        })
        .to_string()
    }
}

impl std::error::Error for Problem {}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.title, self.detail)
    }
}
