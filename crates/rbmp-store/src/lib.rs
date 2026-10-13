#[cfg(feature = "duckdb-store")]
pub mod duck;
pub mod journal;
#[cfg(feature = "duckdb-store")]
pub mod query;
#[cfg(feature = "duckdb-store")]
pub mod schema;
#[cfg(feature = "duckdb-store")]
pub mod writer;

#[cfg(feature = "duckdb-store")]
pub use duck::RouteStore;
pub use journal::{
    BmpJournal, BmpJournalControlRecord, BmpJournalObservation, JournalTransportGap,
    PrefixHistoryItem, ReplayOutcome, ReplayReport, RibEventJournal, should_clear_transport_gap,
};
