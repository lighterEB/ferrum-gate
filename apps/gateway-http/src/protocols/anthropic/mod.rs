// Anthropic Messages API protocol implementation.
// These modules define request parsing, response formatting, and stream processing
// for the Anthropic Messages API wire format. They will be wired into the
// /v1/messages endpoint handler once the route is added.
#![allow(dead_code)]

pub mod request;
pub mod response;
pub mod streaming;
