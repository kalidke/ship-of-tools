//! The folded comparison of the home folder and the repository top, a folder index whose text leads out, and a target
//! through a symlinked folder that the refusal names. The fixture and the request helpers are `links_tests.rs`'s.

use super::tests::{body_text, fixture, get, git, r3, serve, widened, write, Fixture};
use super::*;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;

fn raw(b: &[u8]) -> &Path {
    Path::new(std::ffi::OsStr::from_bytes(b))
}

/// A site whose bounds name `home` as the home folder, with the fixture's mount folder.
fn site_of(f: &Fixture, home: PathBuf) -> Site {
    Site {
        content_root: f.repo.join("site"),
        url_root: f.repo.clone(),
        bounds: Bounds { home: Some(home), ..f.bounds.clone() },
        git: None,
    }
}

/// A site opened the way a folder's own page is served: the content root is its url root, with git attached.
async fn site_root(f: &Fixture) -> Site {
    let mut site = Site::open(f.repo.join("site"), f.repo.clone(), false).await;
    site.bounds = f.bounds.clone();
    site
}

#[test]
fn starts_with_folded_is_equal_or_folded_by_whole_component() {
    assert!(starts_with_folded(Path::new("/a/b/c"), Path::new("/a/b")));
    assert!(starts_with_folded(Path::new("/a/b"), Path::new("/a/b")));
    assert!(starts_with_folded(Path::new("/A/B/c"), Path::new("/a/b")));
    assert!(starts_with_folded(Path::new("/x/Ä/f"), Path::new("/x/ä")));
}

#[test]
fn starts_with_folded_is_not_a_string_prefix() {
    assert!(!starts_with_folded(Path::new("/a/bc"), Path::new("/a/b")));
    assert!(!starts_with_folded(Path::new("/a/BC"), Path::new("/a/b")));
    assert!(!starts_with_folded(Path::new("/a"), Path::new("/a/b")));
}

#[test]
fn starts_with_folded_compares_a_name_that_is_not_utf8_exactly() {
    assert!(starts_with_folded(raw(b"/x/\xff/f"), raw(b"/x/\xff")));
    assert!(!starts_with_folded(raw(b"/x/A\xff/f"), raw(b"/x/a\xff")));
}

#[test]
fn base_for_refuses_the_home_folder_under_another_case_spelling() {
    let f = fixture();
    assert_eq!(f.bounds.base_for(&f.base.join("mnt/home/u/notes.txt")), Err("in the home folder"));
    assert_eq!(f.bounds.base_for(&f.base.join("mnt/Home/U/notes.txt")), Err("in the home folder"));
}

#[test]
fn judge_refuses_a_link_into_the_home_folder_under_another_case_spelling() {
    let f = fixture();
    let at = f.repo.join("h");
    symlink(f.base.join("mnt/home/u"), &at).unwrap();
    let site = site_of(&f, f.base.join("mnt/Home/U"));
    assert_eq!(judge(&site, &at, Path::new("h"), None).map(|_| ()), Err("the link points outside the bounds"));
}

#[test]
fn judge_refuses_a_link_into_the_repository_top_under_another_case_spelling() {
    let f = fixture();
    let site = site_of(&f, f.base.join("mnt/home/u"));
    let top = f.base.join("mnt/SHARE");
    let at = f.repo.join("data");
    assert_eq!(
        judge(&site, &at, Path::new("data"), Some(&top)).map(|_| ()),
        Err("the link points into the repository")
    );
}

#[test]
fn judge_refuses_a_file_in_the_repository_top_under_another_case_spelling() {
    let f = fixture();
    let site = site_of(&f, f.base.join("mnt/home/u"));
    let top = f.base.join("mnt/share/RESULTS/r1");
    let at = f.repo.join("data");
    let q = at.join("results/r1/movie.mp4");
    assert_eq!(judge(&site, &q, Path::new("data"), Some(&top)).map(|_| ()), Err("in the repository"));
}

/// A folder index that links straight out is refused with the link's R3 answer, as it always was.
#[tokio::test]
async fn a_folder_index_linking_out_is_r3_before_it_is_looked_up() {
    let f = fixture();
    std::fs::create_dir_all(f.repo.join("site/dir")).unwrap();
    symlink(f.base.join("outside/secret.txt"), f.repo.join("site/dir/index.html")).unwrap();
    git(&f.repo, &["add", "site/dir/index.html"]);
    let site = site_root(&f).await;
    let refusal = resolve_and_open(&site, "dir/").err().expect("a refusal");
    assert_eq!(refusal.status, "403 Forbidden");
    assert_eq!(refusal.body, r3("dir", "dir/index.html"));
}

/// The index's own text leads out of the site, though the file it names resolves inside the site through a link
/// below the folder: the text decides before the lookup, so the answer is R3 and no file is served.
#[tokio::test]
async fn a_folder_index_whose_text_leads_out_is_r3_though_it_resolves_inside() {
    let f = fixture();
    std::fs::create_dir_all(f.repo.join("site/sub")).unwrap();
    std::fs::create_dir_all(f.repo.join("site/dir")).unwrap();
    write(&f.repo.join("site/x"), b"inside");
    symlink(f.repo.join("site/sub"), f.base.join("outside/ln")).unwrap();
    symlink(f.base.join("outside/ln/../x"), f.repo.join("site/dir/index.html")).unwrap();
    git(&f.repo, &["add", "site/dir/index.html"]);
    let site = site_root(&f).await;
    let refusal = resolve_and_open(&site, "dir/").err().expect("a refusal");
    assert_eq!(refusal.status, "403 Forbidden");
    assert_eq!(refusal.body, r3("dir", "dir/index.html"));
}

/// A target that runs through a symlinked folder under a mount folder is refused, and the refusal names that folder
/// rather than suggesting the share is not mounted.
#[tokio::test]
async fn a_target_through_a_symlinked_folder_under_a_mount_folder_names_that_folder() {
    let f = fixture();
    symlink(&f.share, f.base.join("mnt/alias")).unwrap();
    symlink(f.base.join("mnt/alias/results"), f.repo.join("via")).unwrap();
    git(&f.repo, &["add", "via"]);
    let (addr, nonce, serial) = serve(widened(&f).await).await;
    let (st, _, body) = get(addr, &format!("/{nonce}/via/r1/movie.mp4"), &[]).await;
    let body = body_text(&body);
    assert_eq!(st, 403, "{body}");
    let named = format!(
        "the link's target runs through the symlinked folder {}/mnt/alias:",
        f.base.display()
    );
    assert!(body.contains(&named), "{body}");
    assert!(!body.contains("is its share mounted"), "{body}");
    remove_root(serial);
}
