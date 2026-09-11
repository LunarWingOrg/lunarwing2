# Synthetic Standard Article

By Test Author | September 1, 2026

This synthetic fixture replaces a real-world homepage snapshot so the
 tree carries no third-party text as an anchor. It exercises the plain
 article path of the readability extractor: a title, a byline, and body
 paragraphs with inline formatting such as **bold emphasis** and *italic emphasis*.

The second paragraph verifies that relative links resolve against the
 page URL. See the [reference documentation](/docs/reference) and the [external example site](https://example.org/external) for contrast.

## Section One: Lists

Ordered lists convert to markdown numbering. The items below are
 stable tokens used by the golden file.

1. First numbered item
2. Second numbered item
3. Third numbered item

Unordered lists convert to bullets:

- Alpha bullet
- Beta bullet
- Gamma bullet

## Section Two: Quotes
> This blockquote must survive conversion as a markdown quote block.

Inline code such as `convert_html_to_markdown` converts to
 backticks. A final paragraph closes the article body.

![Fixture chart](/img/fixture-chart.png)

*Figure 1: the fixture chart caption*
