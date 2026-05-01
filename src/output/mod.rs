//! Output adapters — bus subscribers that push data to external systems.
//!
//! Each adapter is a background task that:
//!   - Reads from the bonsai graph or broadcast channel (read-only)
//!   - Transforms data to vendor format
//!   - Pushes via configured transport (HTTP, gRPC, etc.)
//!   - Credentials via vault, audit-logged, environment-scoped
//!
//! This module currently holds one-off adapters. The full `OutputAdapter` trait
//! (T6-1) will be layered on top when Sprint 7 lands; these implementations will
//! be refactored to implement it at that point.

pub mod servicenow_em;
