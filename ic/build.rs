use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let root = PathBuf::from(&manifest_dir);

    // ── Embed registry manifests ────────────────────────────────────────
    embed_registry_catalog(&root);

    // ── Embed bundled skills ────────────────────────────────────────────
    embed_skills(&root);

    // ── Embed build identity ────────────────────────────────────────────
    embed_build_identity(&root);

    // ── Track migrations ────────────────────────────────────────────────
    // refinery's embed_migrations!("migrations") include_str!s each file it
    // finds, so edits are tracked, but a NEW migration is invisible to cargo:
    // a migration-only change would leave a reused target dir (tenant
    // upgrades) with a binary that lacks it, while clean CI builds pass.
    // Watching the directory reruns this script, which recompiles the crate
    // so the macro rescans it.
    println!("cargo:rerun-if-changed=migrations");
}

/// Collect all registry manifests into a single JSON blob at compile time.
///
/// Output: `$OUT_DIR/embedded_catalog.json` with structure:
/// ```json
/// { "tools": [...], "channels": [...], "bundles": {...} }
/// ```
fn embed_registry_catalog(root: &Path) {
    use std::fs;

    let registry_dir = root.join("registry");

    // Rerun if the bundles file changes (per-file watches for tools/channels
    // are emitted inside collect_json_files to track content changes reliably).
    println!("cargo:rerun-if-changed=registry/_bundles.json");
    // Per-file watches miss NEW manifests: cargo reruns a build script only
    // when a watched path changes, so an added tool/channel would never reach
    // the embedded catalog of a reused target dir (tenant upgrades), while
    // clean CI builds pass. The directory watch catches additions.
    println!("cargo:rerun-if-changed=registry");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let out_path = out_dir.join("embedded_catalog.json");

    if !registry_dir.is_dir() {
        // No registry dir: write empty catalog
        fs::write(
            &out_path,
            r#"{"tools":[],"channels":[],"mcp_servers":[],"bundles":{"bundles":{}}}"#,
        )
        .unwrap();
        return;
    }

    let mut tools = Vec::new();
    let mut channels = Vec::new();
    let mut mcp_servers = Vec::new();

    // Collect tool manifests
    let tools_dir = registry_dir.join("tools");
    if tools_dir.is_dir() {
        collect_json_files(&tools_dir, &mut tools);
    }

    // Collect channel manifests
    let channels_dir = registry_dir.join("channels");
    if channels_dir.is_dir() {
        collect_json_files(&channels_dir, &mut channels);
    }

    // Collect MCP server manifests
    let mcp_servers_dir = registry_dir.join("mcp-servers");
    if mcp_servers_dir.is_dir() {
        collect_json_files(&mcp_servers_dir, &mut mcp_servers);
    }

    // Read bundles
    let bundles_path = registry_dir.join("_bundles.json");
    let bundles_raw = if bundles_path.is_file() {
        fs::read_to_string(&bundles_path).unwrap_or_else(|_| r#"{"bundles":{}}"#.to_string())
    } else {
        r#"{"bundles":{}}"#.to_string()
    };

    // Build the combined JSON
    let catalog = format!(
        r#"{{"tools":[{}],"channels":[{}],"mcp_servers":[{}],"bundles":{}}}"#,
        tools.join(","),
        channels.join(","),
        mcp_servers.join(","),
        bundles_raw,
    );

    fs::write(&out_path, catalog).unwrap();
}

/// Collect all `skills/*/SKILL.md` files into an embedded JSON blob.
///
/// Output: `$OUT_DIR/embedded_skills.json` — a JSON array of `{"name": "...", "content": "..."}`.
/// These are loaded at runtime as bundled skills (lowest discovery priority, Trusted trust level).
fn embed_skills(root: &Path) {
    use std::fs;

    let skills_dir = root.join("skills");

    // Rerun when any skill changes
    println!("cargo:rerun-if-changed=skills");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap()); // safety: build script panics on failure
    let out_path = out_dir.join("embedded_skills.json");

    if !skills_dir.is_dir() {
        fs::write(&out_path, "[]").unwrap(); // safety: build script
        return;
    }

    let mut skills: Vec<String> = Vec::new();

    let mut entries: Vec<_> = fs::read_dir(&skills_dir)
        .unwrap() // safety: build script
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let skill_md = entry.path().join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        // Emit per-file watch
        println!("cargo:rerun-if-changed={}", skill_md.display());

        let name = entry.file_name().to_string_lossy().to_string();
        if let Ok(content) = fs::read_to_string(&skill_md) {
            // Escape for JSON embedding
            let name_json = serde_json::to_string(&name).unwrap(); // safety: build script
            let content_json = serde_json::to_string(&content).unwrap(); // safety: build script
            skills.push(format!(
                r#"{{"name":{},"content":{}}}"#,
                name_json, content_json
            ));
        }
    }

    let catalog = format!("[{}]", skills.join(","));
    fs::write(&out_path, catalog).unwrap(); // safety: build script
}

/// Read all .json files from a directory and push their raw contents into `out`.
fn collect_json_files(dir: &Path, out: &mut Vec<String>) {
    use std::fs;

    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().is_file() && e.path().extension().and_then(|x| x.to_str()) == Some("json")
        })
        .collect();

    // Sort for deterministic output
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        // Emit per-file watch so Cargo reruns when file contents change
        println!("cargo:rerun-if-changed={}", entry.path().display());
        if let Ok(content) = fs::read_to_string(entry.path()) {
            out.push(content);
        }
    }
}

/// Embed the build identity the startup banner prints (WI#870): the source
/// commit and the build time.
///
/// The package version alone cannot distinguish two builds of the same
/// version, so a tester reading `daemon.log` could not tell which build was
/// running. The commit answers "what code is this"; the build time separates
/// rebuilds of one commit (a dirty tree, or two builds of the same checkout).
///
/// Overrides, both optional:
/// - `LUNARWING_BUILD_COMMIT` — for builds without a `.git` directory (the
///   container image, a `git archive` tree), or when the build account cannot
///   read the checkout. Defaults to the checkout's HEAD, `-dirty` when
///   tracked files under the crate differ, and `unknown` otherwise.
/// - `SOURCE_DATE_EPOCH` — pins the build time for reproducible builds.
///
/// Nothing here can fail the build: a missing git or clock degrades to
/// `unknown` / `0`.
fn embed_build_identity(root: &Path) {
    let commit = env::var("LUNARWING_BUILD_COMMIT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| derive_git_commit(root));

    let epoch = env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|elapsed| elapsed.as_secs())
        })
        .unwrap_or(0);

    println!("cargo:rustc-env=LUNARWING_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=LUNARWING_BUILD_EPOCH={epoch}");
    println!("cargo:rerun-if-env-changed=LUNARWING_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    // Cargo stops using its default "any package file changed" heuristic once
    // a build script emits rerun-if-changed, so the sources have to be
    // watched explicitly or an edited tree keeps a stale banner. HEAD is
    // watched because a checkout/commit moves the printed commit; the index is
    // not, so a plain `git add` does not force a relink.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    if let Some(head) = git(root, &["rev-parse", "--absolute-git-dir"])
        .map(|dir| PathBuf::from(dir).join("HEAD"))
        .filter(|head| head.exists())
    {
        println!("cargo:rerun-if-changed={}", head.display());
    }
}

/// Short HEAD of the checkout, suffixed `-dirty` when tracked files under the
/// crate have uncommitted changes. `unknown` when git cannot answer.
fn derive_git_commit(root: &Path) -> String {
    let revision = match git(root, &["rev-parse", "--short=8", "HEAD"]) {
        Some(revision) if !revision.is_empty() => revision,
        _ => return "unknown".to_string(),
    };
    // Scoped to the crate and tracked files only: untracked build output must
    // not mark the tree dirty, and `ic/vendor/` is large enough that a
    // repo-wide scan would slow every rebuild.
    let dirty = git(
        root,
        &["status", "--porcelain", "--untracked-files=no", "--", "."],
    )
    .is_none_or(|status| !status.is_empty());
    if dirty {
        format!("{revision}-dirty")
    } else {
        revision
    }
}

/// Run one git command in `root`, returning trimmed stdout on success.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_string())
}
