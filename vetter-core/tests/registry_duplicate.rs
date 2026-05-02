//! Asserts that registering two parsers with the same `name()` panics.
//! Lives in its own test binary because the panic poisons the registry
//! mutex; isolating it keeps the panic out of the way of other tests.

use vetter_core::parsers::{self, noop::NoopParser};

#[test]
#[should_panic(expected = "duplicate parser registration")]
fn duplicate_registration_panics() {
    parsers::register(Box::new(NoopParser));
    parsers::register(Box::new(NoopParser));
}
