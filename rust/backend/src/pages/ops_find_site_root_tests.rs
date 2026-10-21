// `docs.open`'s site-root walk, tested directly against real temp
// directories: `find_site_root` is a pure path-and-filesystem function, so
// this is the cheap place to pin each bound. The handler end to end, over
// the wire, is `rust/backend/tests/site_root.rs`.
use super::{find_site_root, is_plain_link, narrow_to_links, root_relative_paths};

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sot-find-site-root-test-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // Canonical, as `docs.open` hands these walks their folders (`canonicalize_and_workspace_root`): a temp folder
    // reached through a symlink (macOS's `/var`) or spelled 8.3 (`RUNNER~1`) is resolved here as it is there, and
    // on Windows takes the `\\?\` form the handler's paths have.
    dir.canonicalize().unwrap()
}

fn page(path: &std::path::Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "<html></html>").unwrap();
}

#[tokio::test]
async fn climbs_through_index_less_dirs_to_a_distant_site_index() {
    // <ws>/site/index.html, subpage two levels down at
    // <ws>/site/guide/api/reference.html — `guide/` and `guide/api/`
    // have no index of their own, matching a typical Documenter tree
    // where only the top has one.
    let ws = scratch("climb");
    let site = ws.join("site");
    let sub = site.join("guide").join("api");
    page(&site.join("index.html"));
    page(&sub.join("reference.html"));

    let root = find_site_root(&sub, &ws, None).await.expect("site root found");
    assert_eq!(root, site, "climbs past two index-less dirs to the site root");

    let _ = std::fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn the_outermost_index_in_the_repository_is_the_site() {
    // A project log: <repo>/log/index.html, a campaign index at
    // <repo>/log/experiments/camp/index.html and no index in
    // `experiments/`. An index page above the repository (<ws>/index.html)
    // is never folded in: the walk ends at the folder holding `.git`.
    let ws = scratch("outermost");
    let repo = ws.join("repo");
    let log = repo.join("log");
    let camp = log.join("experiments").join("camp");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    page(&ws.join("index.html"));
    page(&log.join("index.html"));
    page(&camp.join("index.html"));

    let root = find_site_root(&camp, &ws, None).await.expect("site root found");
    assert_eq!(root, log, "across the index-less experiments/, and not past the repository top");

    let _ = std::fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn a_folder_holding_home_is_never_the_site_root() {
    // <home>/index.html and a page in <home>/downloads/bundle/ with no
    // index on the way: the page's own folder is its site, never the
    // home folder; and a page directly in the home folder has none.
    let ws = scratch("home");
    let home = ws.join("home");
    let bundle = home.join("downloads").join("bundle");
    page(&home.join("index.html"));
    page(&bundle.join("page.html"));

    assert_eq!(find_site_root(&bundle, &ws, Some(&home)).await, Some(bundle.clone()));
    assert_eq!(find_site_root(&home, &ws, Some(&home)).await, None);

    let _ = std::fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn no_index_up_to_the_workspace_root_serves_the_page_folder() {
    // A standalone page with a SIBLING assets/ dir and no index.html
    // anywhere above it up to the workspace root: its own folder.
    let ws = scratch("no-index");
    let page_dir = ws.join("journal");
    std::fs::create_dir_all(ws.join("assets")).unwrap();
    page(&page_dir.join("2026-09-01.html"));

    assert_eq!(find_site_root(&page_dir, &ws, None).await, Some(page_dir.clone()));

    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn root_relative_paths_are_cut_at_the_quote_query_and_fragment() {
    let html = "<link href=\"/css/a.css?v=2\"><script src='/js/b.js#x'></script><img src=\"//cdn/x.png\"><a href=\"/\">";
    assert_eq!(root_relative_paths(html), ["css/a.css", "js/b.js", ""], "document order; no protocol-relative");
    assert_eq!(root_relative_paths("<script src=\"/a\"><link href=\"/b\">"), ["a", "b"], "not tag-type order");
    assert!(root_relative_paths("<a href=\"rel/x.html\">").is_empty());
}

#[test]
fn only_plain_names_are_links_that_narrow() {
    assert!(is_plain_link("assets/a.js"));
    for bad in ["", "a/", "a//b", "../x", "a/../x", "./a", "C:/Users/x", "..\\x", "a\\b", "c:x", "a/."] {
        assert!(!is_plain_link(bad), "{bad:?}");
    }
}

#[tokio::test]
async fn a_root_relative_page_is_rooted_where_its_first_link_resolves() {
    // <ws>/index.html (a source index) above <ws>/dist/ (the build, `/assets/` links).
    let ws = scratch("narrow");
    let dist = ws.join("dist");
    page(&ws.join("index.html"));
    page(&dist.join("index.html"));
    page(&dist.join("assets").join("a.js"));
    let links = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    assert_eq!(narrow_to_links(ws.clone(), &dist, &links(&["assets/a.js"])).await, dist);
    assert_eq!(narrow_to_links(ws.clone(), &dist, &links(&["nowhere.js"])).await, ws, "no folder resolves it");
    assert_eq!(narrow_to_links(ws.clone(), &dist, &links(&["../index.html"])).await, ws, "a `..` link is ignored");
    assert_eq!(narrow_to_links(ws.clone(), &dist, &[]).await, ws);

    let _ = std::fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn site_root_exactly_at_the_workspace_root_stops_there() {
    // <ws>/index.html with the page directly inside <ws> — the walk
    // must find <ws> itself and never look above it.
    let ws = scratch("at-root");
    page(&ws.join("index.html"));
    page(&ws.join("page.html"));

    let root = find_site_root(&ws, &ws, None).await.expect("site root found");
    assert_eq!(root, ws, "the workspace root itself is the site root");

    let _ = std::fs::remove_dir_all(&ws);
}
