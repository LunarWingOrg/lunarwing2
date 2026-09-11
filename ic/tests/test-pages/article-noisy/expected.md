# Synthetic Noisy Page: Article Extraction

By Fixture Reporter

The purpose of this fixture is readability's junk-stripping behavior.
 The page wraps a normal article in the kind of container clutter that
 real pages carry: a masthead, a primary navigation list, a breaking-news
 banner, a sidebar of related links, and a footer of legal links. Only
 the article body below should survive extraction.

The article itself contains three paragraphs. This first one
 introduces the topic with enough plain text to satisfy the extractor's
 content threshold. It mentions the **extraction sentinel** phrase used by the assertions.

The second paragraph includes a related-links block that sits inside
 the article container but outside the content column. A converter that
 fails to strip it would leak the sidebar strings into the output.

The third and final paragraph closes with a sign-off sentence so the
 extracted article has a determinate ending.
