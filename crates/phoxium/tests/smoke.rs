//! Smoke test: the library API is reachable from outside the crate.

use phoxium::{cli, scaling};

#[test]
fn usage_lists_the_main_options() {
    assert!(cli::USAGE.starts_with("phoxium:"));
    assert!(cli::USAGE.contains("--window <text>"));
}

#[test]
fn divisor_parser_is_public() {
    assert_eq!(scaling::parse_divisor("3").unwrap(), (3, 3));
    assert_eq!(scaling::parse_divisor("2x4").unwrap(), (2, 4));
}
