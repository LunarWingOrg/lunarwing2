#![allow(missing_docs)]

#[cfg(target_arch = "wasm32")]
use html_to_markdown_rs::DEFAULT_WASM_MAX_INPUT_SIZE;
use html_to_markdown_rs::{ConversionError, ConversionOptions, ConversionOptionsUpdate, convert};

#[test]
fn should_return_the_observed_size_when_input_exceeds_the_configured_limit() {
    let html = "<p>é</p>";
    let options = ConversionOptions {
        max_input_size: Some(8),
        ..ConversionOptions::default()
    };

    let error = convert(html, options).expect_err("nine input bytes should exceed an eight-byte limit");

    assert_eq!(
        error.to_string(),
        "Input size 9 bytes exceeds the configured maximum of 8 bytes"
    );
    assert!(matches!(
        &error,
        ConversionError::InputTooLarge {
            observed_size: 9,
            max_size: 8
        }
    ));
}

#[test]
fn should_accept_input_exactly_at_the_configured_limit() {
    let html = "<p>x</p>";
    let options = ConversionOptions {
        max_input_size: Some(html.len() as u64),
        ..ConversionOptions::default()
    };

    let result = convert(html, options).expect("input at the byte limit should be accepted");

    assert_eq!(result.content.as_deref(), Some("x\n"));
}

#[test]
fn should_allow_callers_to_disable_the_input_limit() {
    let html = "<p>x</p>";
    let options = ConversionOptions {
        max_input_size: None,
        ..ConversionOptions::default()
    };

    let result = convert(html, options).expect("an absent limit should accept the input");

    assert_eq!(result.content.as_deref(), Some("x\n"));
}

#[test]
fn should_configure_the_limit_through_the_builder_and_partial_updates() {
    let mut options = ConversionOptions::builder().max_input_size(Some(16)).build();
    assert_eq!(options.max_input_size, Some(16));

    options.apply_update(ConversionOptionsUpdate {
        max_input_size: Some(None),
        ..ConversionOptionsUpdate::default()
    });
    assert_eq!(options.max_input_size, None);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn should_leave_the_native_default_unbounded() {
    assert_eq!(ConversionOptions::default().max_input_size, None);
}

#[test]
#[cfg(target_arch = "wasm32")]
fn should_default_wasm_to_two_mebibytes() {
    assert_eq!(
        ConversionOptions::default().max_input_size,
        Some(DEFAULT_WASM_MAX_INPUT_SIZE)
    );
    assert_eq!(DEFAULT_WASM_MAX_INPUT_SIZE, 2 * 1024 * 1024);
}
