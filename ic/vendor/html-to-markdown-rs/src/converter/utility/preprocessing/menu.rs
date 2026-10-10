use std::borrow::Cow;

use super::markup::{find_tag_end, matches_end_tag_start, matches_tag_start};
use super::raw_text::{opens_a_tag, skip_opaque_region};

pub const PRESERVED_MENU_ATTRIBUTE: &str = "data-html-to-markdown-preserved-menu";
const PRESERVED_MENU_MARKER: &str = " data-html-to-markdown-preserved-menu=\"\"";

/// Normalize parser-broken `<menu>` elements to `<ul>` before parsing.
///
/// ~keep The bundled parser foster-parents `<li>` children out of `<menu>`, so the converter
/// ~keep cannot preserve a menu nested in a list item after the DOM has been built (issue #657).
pub fn normalize_menu_elements(input: &str, preserve_menu: bool) -> Cow<'_, str> {
    let mut replacements = menu_name_replacements(input.as_bytes());
    if replacements.is_empty() {
        return Cow::Borrowed(input);
    }
    replacements.sort_unstable_by_key(|&(start, _, _)| start);

    let mut output = String::with_capacity(input.len());
    let mut last = 0;
    for (start, end, opening) in replacements {
        output.push_str(&input[last..start]);
        output.push_str("ul");
        if preserve_menu && opening {
            output.push(' ');
            output.push_str(PRESERVED_MENU_ATTRIBUTE);
            output.push_str("=\"\"");
        }
        last = end;
    }
    output.push_str(&input[last..]);
    Cow::Owned(output)
}

pub fn restore_preserved_menu_elements(input: &str) -> Cow<'_, str> {
    let bytes = input.as_bytes();
    let mut idx = 0;
    let mut menus = Vec::new();
    let mut replacements = Vec::new();
    while idx < bytes.len() {
        if bytes[idx] != b'<' {
            idx += 1;
            continue;
        }
        if let Some(region_end) = skip_opaque_region(bytes, idx) {
            idx = region_end;
            continue;
        }
        if matches_end_tag_start(bytes, idx + 1, b"ul") {
            let name_start = idx + 2;
            let name_end = name_start + 2;
            if menus.pop().unwrap_or(false) {
                replacements.push((name_start, name_end, "menu"));
            }
            idx = find_tag_end(bytes, name_end).unwrap_or(bytes.len());
            continue;
        }
        if matches_tag_start(bytes, idx + 1, b"ul") {
            let name_start = idx + 1;
            let name_end = name_start + 2;
            let tag_end = find_tag_end(bytes, name_end).unwrap_or(bytes.len());
            let marker = input[idx..tag_end]
                .find(PRESERVED_MENU_MARKER)
                .map(|offset| idx + offset);
            let preserved = marker.is_some();
            if let Some(marker_start) = marker {
                replacements.push((name_start, name_end, "menu"));
                replacements.push((marker_start, marker_start + PRESERVED_MENU_MARKER.len(), ""));
            }
            if !bytes[idx..tag_end].ends_with(b"/>") {
                menus.push(preserved);
            }
            idx = tag_end;
            continue;
        }
        if opens_a_tag(bytes, idx) {
            idx = find_tag_end(bytes, idx + 1).unwrap_or(bytes.len());
            continue;
        }
        idx += 1;
    }
    if replacements.is_empty() {
        return Cow::Borrowed(input);
    }
    replacements.sort_unstable_by_key(|&(start, _, _)| start);
    let mut output = String::with_capacity(input.len());
    let mut last = 0;
    for (start, end, replacement) in replacements {
        output.push_str(&input[last..start]);
        output.push_str(replacement);
        last = end;
    }
    output.push_str(&input[last..]);
    Cow::Owned(output)
}

struct OpenMenuTag {
    name_start: usize,
    name_end: usize,
    has_list_item: bool,
    nested_in_list_item: bool,
}

#[derive(Default)]
struct MenuTagScan {
    list_item_depth: usize,
    menus: Vec<OpenMenuTag>,
    replacements: Vec<(usize, usize, bool)>,
}

impl MenuTagScan {
    fn scan_menu(&mut self, bytes: &[u8], idx: usize) -> Option<usize> {
        if matches_end_tag_start(bytes, idx + 1, b"menu") {
            let close_start = idx + 2;
            let close_end = close_start + b"menu".len();
            if let Some(menu) = self
                .menus
                .pop()
                .filter(|menu| menu.nested_in_list_item && menu.has_list_item)
            {
                self.replacements.push((menu.name_start, menu.name_end, true));
                self.replacements.push((close_start, close_end, false));
            }
            return Some(find_tag_end(bytes, close_end).unwrap_or(bytes.len()));
        }
        if !matches_tag_start(bytes, idx + 1, b"menu") {
            return None;
        }
        let name_start = idx + 1;
        let name_end = name_start + b"menu".len();
        let tag_end = find_tag_end(bytes, name_end).unwrap_or(bytes.len());
        if !bytes[idx..tag_end].ends_with(b"/>") {
            self.menus.push(OpenMenuTag {
                name_start,
                name_end,
                has_list_item: false,
                nested_in_list_item: self.list_item_depth > 0,
            });
        }
        Some(tag_end)
    }

    fn track_list_item(&mut self, bytes: &[u8], idx: usize) {
        if matches_tag_start(bytes, idx + 1, b"li") {
            if let Some(menu) = self.menus.last_mut() {
                menu.has_list_item = true;
            }
            self.list_item_depth += 1;
        } else if matches_end_tag_start(bytes, idx + 1, b"li") {
            self.list_item_depth = self.list_item_depth.saturating_sub(1);
        }
    }
}

fn menu_name_replacements(bytes: &[u8]) -> Vec<(usize, usize, bool)> {
    let mut idx = 0;
    let mut scan = MenuTagScan::default();
    while idx < bytes.len() {
        if bytes[idx] != b'<' {
            idx += 1;
            continue;
        }
        if let Some(region_end) = skip_opaque_region(bytes, idx) {
            idx = region_end;
            continue;
        }
        if let Some(tag_end) = scan.scan_menu(bytes, idx) {
            idx = tag_end;
            continue;
        }
        scan.track_list_item(bytes, idx);
        if opens_a_tag(bytes, idx) {
            idx = find_tag_end(bytes, idx + 1).unwrap_or(bytes.len());
            continue;
        }
        idx += 1;
    }
    scan.replacements
}
