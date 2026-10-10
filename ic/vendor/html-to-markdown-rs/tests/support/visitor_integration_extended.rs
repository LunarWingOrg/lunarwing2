#[test]
fn test_visitor_blockquote() {
    #[derive(Debug, Default)]
    struct BlockquoteVisitor;

    impl HtmlVisitor for BlockquoteVisitor {
        fn visit_blockquote(&mut self, _ctx: &NodeContext, content: &str, _depth: usize) -> VisitResult {
            VisitResult::Custom(format!("[QUOTE:{}]", content.trim()))
        }
    }

    let html = r"<blockquote>This is a quote</blockquote>";
    let visitor = Arc::new(Mutex::new(BlockquoteVisitor));
    let result = convert(html, None, Some(visitor))
        .expect("conversion failed")
        .content
        .unwrap_or_default();

    assert!(
        result.contains("[QUOTE:This is a quote]"),
        "Should contain custom blockquote format, got: {result}"
    );
}

#[test]
fn test_visitor_inline_formatting() {
    #[derive(Debug, Default)]
    struct FormattingVisitor;

    impl HtmlVisitor for FormattingVisitor {
        fn visit_strong(&mut self, _ctx: &NodeContext, text: &str) -> VisitResult {
            VisitResult::Custom(format!("[STRONG:{text}]"))
        }

        fn visit_emphasis(&mut self, _ctx: &NodeContext, text: &str) -> VisitResult {
            VisitResult::Custom(format!("[EM:{text}]"))
        }

        fn visit_strikethrough(&mut self, _ctx: &NodeContext, text: &str) -> VisitResult {
            VisitResult::Custom(format!("[DEL:{text}]"))
        }
    }

    let html = r"<p><strong>bold</strong> <em>italic</em> <del>struck</del></p>";
    let visitor = Arc::new(Mutex::new(FormattingVisitor));
    let result = convert(html, None, Some(visitor))
        .expect("conversion failed")
        .content
        .unwrap_or_default();

    assert!(result.contains("[STRONG:bold]"), "Should see strong, got: {result}");
    assert!(result.contains("[EM:italic]"), "Should see emphasis, got: {result}");
    assert!(
        result.contains("[DEL:struck]"),
        "Should see strikethrough, got: {result}"
    );
}

#[test]
fn test_no_double_visit_in_links() {
    #[derive(Debug, Default)]
    struct CountingVisitor {
        text_visits: usize,
    }

    impl HtmlVisitor for CountingVisitor {
        fn visit_text(&mut self, _ctx: &NodeContext, _text: &str) -> VisitResult {
            self.text_visits += 1;
            VisitResult::Continue
        }

        fn visit_link(&mut self, _ctx: &NodeContext, _href: &str, _text: &str, _title: Option<&str>) -> VisitResult {
            VisitResult::Continue
        }
    }

    let html = r#"<a href="/url">link text</a>"#;
    let visitor = Arc::new(Mutex::new(CountingVisitor::default()));
    let _result = convert(html, None, Some(visitor.clone())).expect("conversion failed");

    assert_eq!(
        visitor.lock().expect("visitor mutex poisoned").text_visits,
        1,
        "Text nodes inside links should only be visited once, got {} visits",
        visitor.lock().expect("visitor mutex poisoned").text_visits
    );
}

#[test]
fn test_no_double_visit_in_headings() {
    #[derive(Debug, Default)]
    struct CountingVisitor {
        text_visits: usize,
    }

    impl HtmlVisitor for CountingVisitor {
        fn visit_text(&mut self, _ctx: &NodeContext, _text: &str) -> VisitResult {
            self.text_visits += 1;
            VisitResult::Continue
        }

        fn visit_heading(&mut self, _ctx: &NodeContext, _level: u32, _text: &str, _id: Option<&str>) -> VisitResult {
            VisitResult::Continue
        }
    }

    let html = r"<h1>heading text</h1>";
    let visitor = Arc::new(Mutex::new(CountingVisitor::default()));
    let _result = convert(html, None, Some(visitor.clone())).expect("conversion failed");

    assert_eq!(
        visitor.lock().expect("visitor mutex poisoned").text_visits,
        1,
        "Text nodes inside headings should only be visited once, got {} visits",
        visitor.lock().expect("visitor mutex poisoned").text_visits
    );
}

/// Test that visitor callbacks work correctly when `skip_images` option is enabled
#[test]
fn test_visitor_with_skip_images() {
    #[derive(Debug, Default)]
    struct SkipImageVisitor {
        image_visits: usize,
    }

    impl HtmlVisitor for SkipImageVisitor {
        fn visit_image(&mut self, _ctx: &NodeContext, _src: &str, _alt: &str, _title: Option<&str>) -> VisitResult {
            self.image_visits += 1;
            VisitResult::Continue
        }
    }

    let html = r#"
        <p>Some text</p>
        <img src="/image1.png" alt="Image 1">
        <img src="/image2.png" alt="Image 2">
        <p>More text</p>
    "#;

    let options = ConversionOptions {
        skip_images: true,
        ..Default::default()
    };

    let visitor = Arc::new(Mutex::new(SkipImageVisitor::default()));
    let result = convert(html, Some(options), Some(visitor))
        .expect("conversion with skip_images and visitor should succeed")
        .content
        .unwrap_or_default();

    assert!(
        !result.contains("!["),
        "skip_images should prevent image markdown in output, got: {result}"
    );
    assert!(
        !result.contains("image1.png"),
        "skip_images should prevent image src in output, got: {result}"
    );

    assert!(
        result.contains("Some text") && result.contains("More text"),
        "Other content should still be present in output, got: {result}"
    );
}

/// Test that the main `convert()` function accepts optional visitor parameter
#[test]
fn test_convert_accepts_visitor_parameter() {
    #[derive(Debug, Default)]
    struct CountingVisitor {
        text_count: usize,
        link_count: usize,
    }

    impl HtmlVisitor for CountingVisitor {
        fn visit_text(&mut self, _ctx: &NodeContext, _text: &str) -> VisitResult {
            self.text_count += 1;
            VisitResult::Continue
        }

        fn visit_link(&mut self, _ctx: &NodeContext, _href: &str, _text: &str, _title: Option<&str>) -> VisitResult {
            self.link_count += 1;
            VisitResult::Continue
        }
    }

    let html = r#"<p>Visit <a href="https://example.com">our site</a> for more info.</p>"#;
    let visitor = Arc::new(Mutex::new(CountingVisitor::default()));

    let _result = convert(html, None, Some(visitor.clone())).expect("convert with visitor should work");

    let borrowed = visitor.lock().expect("visitor mutex poisoned");
    assert!(
        borrowed.text_count >= 2,
        "Should visit text nodes, got {} visits",
        borrowed.text_count
    );
    assert_eq!(
        borrowed.link_count, 1,
        "Should visit exactly 1 link, got {}",
        borrowed.link_count
    );
}

/// Test visitor + `inline_images` feature combination
///
/// In v3, `convert()` handles inline-image extraction via `ConversionResult.images`,
/// and `convert_with_visitor()` handles visitor callbacks. We verify both paths
/// work on the same HTML.
#[cfg(feature = "inline-images")]
#[test]
fn test_convert_with_inline_images_accepts_visitor() {
    #[derive(Debug, Default)]
    struct ImageTrackingVisitor {
        images_seen: usize,
    }

    impl HtmlVisitor for ImageTrackingVisitor {
        fn visit_image(&mut self, _ctx: &NodeContext, src: &str, _alt: &str, _title: Option<&str>) -> VisitResult {
            if !src.starts_with("data:") {
                self.images_seen += 1;
            }
            VisitResult::Continue
        }
    }

    let html = r#"
        <h1>Test Page</h1>
        <img src="/image.png" alt="Test Image">
        <p>Some content</p>
    "#;

    let visitor = Arc::new(Mutex::new(ImageTrackingVisitor::default()));
    let markdown = convert(html, None, Some(visitor.clone()))
        .expect("convert should work")
        .content
        .unwrap_or_default();

    assert_eq!(
        visitor.lock().expect("visitor mutex poisoned").images_seen,
        1,
        "Visitor should count 1 non-data-uri image"
    );

    assert!(!markdown.is_empty(), "Should produce markdown output");
}

/// Test visitor + metadata: visitor callbacks fire and metadata is collected.
///
/// In v3, `convert()` always extracts metadata into `ConversionResult.metadata`,
/// and `convert_with_visitor()` handles visitor callbacks. We verify both paths
/// work on the same HTML.
#[cfg(feature = "metadata")]
#[test]
fn test_visitor_and_metadata_both_work() {
    #[derive(Debug, Default)]
    struct MetadataAwareVisitor {
        heading_count: usize,
        link_count: usize,
    }

    impl HtmlVisitor for MetadataAwareVisitor {
        fn visit_heading(&mut self, _ctx: &NodeContext, _level: u32, _text: &str, _id: Option<&str>) -> VisitResult {
            self.heading_count += 1;
            VisitResult::Continue
        }

        fn visit_link(&mut self, _ctx: &NodeContext, _href: &str, _text: &str, _title: Option<&str>) -> VisitResult {
            self.link_count += 1;
            VisitResult::Continue
        }
    }

    let html = r#"
        <html>
        <head><title>Test Page</title></head>
        <body>
            <h1>Main Title</h1>
            <p>Visit <a href="https://example.com">our site</a>.</p>
            <h2>Section</h2>
            <p>More <a href="/page">links</a> here.</p>
        </body>
        </html>
    "#;

    let visitor = Arc::new(Mutex::new(MetadataAwareVisitor::default()));
    let markdown = convert(html, None, Some(visitor.clone()))
        .expect("convert should work")
        .content
        .unwrap_or_default();

    let borrowed = visitor.lock().expect("visitor mutex poisoned");
    assert!(
        borrowed.heading_count >= 2,
        "Visitor should see at least 2 headings, got {}",
        borrowed.heading_count
    );
    assert_eq!(
        borrowed.link_count, 2,
        "Visitor should see 2 links, got {}",
        borrowed.link_count
    );
    assert!(!markdown.is_empty(), "Should produce markdown output");
    drop(borrowed);

    let result = html_to_markdown_rs::convert(html, None).expect("convert should work");
    let metadata = result.metadata;

    assert_eq!(
        metadata.document.title,
        Some("Test Page".to_string()),
        "Metadata should extract title"
    );
    assert!(
        metadata.headers.len() >= 2,
        "Metadata should extract at least 2 headers, got {}",
        metadata.headers.len()
    );
    assert_eq!(
        metadata.links.len(),
        2,
        "Metadata should extract 2 links, got {}",
        metadata.links.len()
    );
}

/// Test visitor + both `inline_images` and `metadata` features together
///
/// In v3, `convert()` handles metadata and inline-image extraction via `ConversionResult`,
/// and `convert_with_visitor()` handles visitor callbacks. We verify both paths
/// work on the same HTML.
#[cfg(all(feature = "inline-images", feature = "metadata"))]
#[test]
fn test_convert_with_all_features_and_visitor() {
    #[derive(Debug, Default)]
    struct ComprehensiveVisitor {
        headings: usize,
        images: usize,
        links: usize,
    }

    impl HtmlVisitor for ComprehensiveVisitor {
        fn visit_heading(&mut self, _ctx: &NodeContext, _level: u32, _text: &str, _id: Option<&str>) -> VisitResult {
            self.headings += 1;
            VisitResult::Continue
        }

        fn visit_image(&mut self, _ctx: &NodeContext, _src: &str, _alt: &str, _title: Option<&str>) -> VisitResult {
            self.images += 1;
            VisitResult::Continue
        }

        fn visit_link(&mut self, _ctx: &NodeContext, _href: &str, _text: &str, _title: Option<&str>) -> VisitResult {
            self.links += 1;
            VisitResult::Continue
        }
    }

    let html = r#"
        <html>
        <body>
            <h1>Gallery</h1>
            <img src="/gallery/image1.jpg" alt="Pic 1">
            <p>See <a href="/more">more</a> content.</p>
            <h2>Details</h2>
            <img src="/gallery/image2.jpg" alt="Pic 2">
            <p>Check <a href="/details">this link</a>.</p>
        </body>
        </html>
    "#;

    let visitor = Arc::new(Mutex::new(ComprehensiveVisitor::default()));
    let markdown = convert(html, None, Some(visitor.clone()))
        .expect("convert should work")
        .content
        .unwrap_or_default();

    let borrowed = visitor.lock().expect("visitor mutex poisoned");
    assert!(
        borrowed.headings >= 2,
        "Visitor should see at least 2 headings, got {}",
        borrowed.headings
    );
    assert_eq!(
        borrowed.images, 2,
        "Visitor should see 2 images, got {}",
        borrowed.images
    );
    assert_eq!(borrowed.links, 2, "Visitor should see 2 links, got {}", borrowed.links);
    drop(borrowed);

    assert!(!markdown.is_empty(), "Should produce markdown output");
}

/// Regression test: image visitor returning Custom with metadata extraction used to panic
/// with an out-of-bounds slice.
///
/// When metadata extraction prepends a YAML frontmatter block to `output`, every element's
/// saved `element_output_start` is offset by the frontmatter length.  If a child visitor
/// then returns Custom and truncates the buffer, the parent's saved offset can point
/// past `output.len()`.
#[test]
fn test_image_visitor_with_metadata_does_not_panic() {
    #[derive(Debug)]
    struct ImageVisitor;

    impl HtmlVisitor for ImageVisitor {
        fn visit_image(&mut self, _ctx: &NodeContext, _src: &str, _alt: &str, _title: Option<&str>) -> VisitResult {
            VisitResult::Custom("![img](rewritten.png)".to_string())
        }
    }

    let html = r#"<html><head><meta name="description" content="x"></head><body><p><img src="a.png" alt="a"></p></body></html>"#;
    let options = ConversionOptions {
        extract_metadata: true,
        ..Default::default()
    };

    let result = convert(html, Some(options), Some(Arc::new(Mutex::new(ImageVisitor))));
    assert!(result.is_ok(), "conversion panicked or errored: {:?}", result.err());
}

/// Regression test: `visit_element_end` returning Custom/Skip with metadata extraction used
/// to produce stale parent offsets and either panic or silently drop subsequent content.
#[test]
fn test_element_end_replacement_with_metadata_preserves_subsequent_content() {
    #[derive(Debug)]
    struct FigureReplacingVisitor;

    impl HtmlVisitor for FigureReplacingVisitor {
        fn visit_element_end(&mut self, ctx: &NodeContext, _content: &str) -> VisitResult {
            if ctx.tag_name == "figure" {
                return VisitResult::Custom("[figure]".to_string());
            }
            VisitResult::Continue
        }
    }

    let html = r#"<html><head><meta name="description" content="x"></head><body><figure><img src="a.png"></figure><p>after</p></body></html>"#;
    let options = ConversionOptions {
        extract_metadata: true,
        ..Default::default()
    };

    let result = convert(html, Some(options), Some(Arc::new(Mutex::new(FigureReplacingVisitor))));
    assert!(result.is_ok(), "conversion panicked or errored: {:?}", result.err());
    assert!(
        result.unwrap().content.unwrap_or_default().contains("after"),
        "content after replaced element should not be lost"
    );
}

/// Regression test for issue #331: visitor receives mismatched start/end events for
/// hyphenated tag names that contain XML-style self-closing children.
///
/// When `<ac:parameter ac:name="foo" />` appears inside a hyphenated custom element, the
/// `repair_with_html5ever` fallback (triggered because the outer tag contains a hyphen) used
/// to re-parse with HTML5 semantics.  HTML5 does NOT honour XML-style self-closing on unknown
/// elements, so `<ac:parameter ... />` was treated as an open tag and subsequent siblings were
/// nested inside it.  That caused `visit_element_start("ac:parameter")` for "foo" to be
/// followed by `visit_element_start("ac:parameter")` for "quux", then both ends in reversed
/// order — violating the expected pre-order/post-order pairing.
#[test]
fn test_issue_331_hyphenated_tags_xml_self_closing_visitor_events() {
    #[derive(Debug, Default)]
    struct EventRecorder {
        events: Vec<String>,
    }

    impl HtmlVisitor for EventRecorder {
        fn visit_element_start(&mut self, ctx: &NodeContext) -> VisitResult {
            self.events.push(format!("start({})", ctx.tag_name));
            VisitResult::Continue
        }

        fn visit_element_end(&mut self, ctx: &NodeContext, _output: &str) -> VisitResult {
            self.events.push(format!("end({})", ctx.tag_name));
            VisitResult::Continue
        }
    }

    let html = r#"
<structured-macro>
  <ac:parameter ac:name="foo" />
  <ac:parameter ac:name="quux">lalaland</ac:parameter>
</structured-macro>
"#;

    let visitor = Arc::new(Mutex::new(EventRecorder::default()));
    let result = convert(html, None, Some(visitor.clone()));
    assert!(result.is_ok(), "conversion should succeed: {:?}", result.err());

    let events = visitor.lock().expect("visitor mutex poisoned").events.clone();

    // ~keep Find the indices of start/end pairs for the two ac:parameter elements.
    // ~keep With correct XML self-closing handling:
    // ~keep   start(ac:parameter)[foo] → end(ac:parameter)[foo] → start(ac:parameter)[quux] → end(ac:parameter)[quux]
    // ~keep With the bug (html5ever treats `/>` as open tag):
    // ~keep   start(ac:parameter)[foo] → start(ac:parameter)[quux] → end(ac:parameter)[quux] → end(ac:parameter)[foo]

    let ac_param_starts: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| e.starts_with("start(ac:parameter)"))
        .map(|(i, _)| i)
        .collect();
    let ac_param_ends: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| e.starts_with("end(ac:parameter)"))
        .map(|(i, _)| i)
        .collect();

    assert_eq!(
        ac_param_starts.len(),
        2,
        "expected exactly 2 ac:parameter start events, got: {events:?}"
    );
    assert_eq!(
        ac_param_ends.len(),
        2,
        "expected exactly 2 ac:parameter end events, got: {events:?}"
    );

    assert!(
        ac_param_starts[0] < ac_param_ends[0],
        "first ac:parameter: start must precede end (got start@{}, end@{}); events: {events:?}",
        ac_param_starts[0],
        ac_param_ends[0],
    );
    assert!(
        ac_param_ends[0] < ac_param_starts[1],
        "first ac:parameter end must precede second ac:parameter start (got end@{}, start@{}); events: {events:?}",
        ac_param_ends[0],
        ac_param_starts[1],
    );
    assert!(
        ac_param_starts[1] < ac_param_ends[1],
        "second ac:parameter: start must precede end (got start@{}, end@{}); events: {events:?}",
        ac_param_starts[1],
        ac_param_ends[1],
    );
}
