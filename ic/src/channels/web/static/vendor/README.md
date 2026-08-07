# Vendored frontend libraries

These libraries are vendored so the web gateway UI works in offline,
air-gapped, and private-network installs (no CDN dependency). Pinned
versions; keep the SRI hashes in `index.html` in sync when upgrading.

| File | Project | Version | License |
| --- | --- | --- | --- |
| `purify.min.js` | [DOMPurify](https://github.com/cure53/DOMPurify) | 3.2.3 | Apache-2.0 OR MPL-2.0 (`DOMPurify.LICENSE`) |
| `marked.umd.min.js` | [marked](https://github.com/markedjs/marked) | 17.0.2 | MIT (`marked.LICENSE`) |
