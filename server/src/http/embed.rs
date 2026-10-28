//! The built page, embedded in the binary and served with content types and cache headers.

use super::router::Part;

/// The parts of the application that serve the page.
pub fn parts() -> Vec<Part> {
    Vec::new()
}
