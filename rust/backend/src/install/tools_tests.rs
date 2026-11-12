use super::*;
use std::cell::RefCell;
use std::ffi::OsString;
use std::path::Path;

/// A folder holding a stub for each name: the search path of a computer that has exactly those tools.
fn bin_with(dir: &Path, names: &[&str]) -> OsString {
    std::fs::create_dir_all(dir).unwrap();
    for n in names {
        std::fs::write(dir.join(n), "#!/bin/sh\n").unwrap();
    }
    dir.into()
}

#[cfg(unix)]
fn names(left: &[&Tool]) -> Vec<&'static str> {
    left.iter().map(|t| t.name).collect()
}

/// An offer run against stand-ins, nobody at the console unless `answer` says: what it asked, ran and said. `has` names
/// the package managers present.
struct Run {
    asked: Vec<String>,
    ran: Vec<Vec<String>>,
    said: Vec<String>,
}

fn run(system: &str, path: &OsStr, has: &[&str], answer: Option<&str>, code: Option<i32>) -> Run {
    let (asked, ran, said) = (RefCell::new(vec![]), RefCell::new(vec![]), RefCell::new(vec![]));
    let seams = Seams {
        ask: &|q| {
            asked.borrow_mut().push(q.to_string());
            answer.map(String::from)
        },
        run: &|argv| {
            ran.borrow_mut().push(argv.to_vec());
            code
        },
        say: &|t| said.borrow_mut().push(t.to_string()),
        has: &|p| has.contains(&p),
    };
    offer(system, path, &seams);
    Run { asked: asked.into_inner(), ran: ran.into_inner(), said: said.into_inner() }
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// Guarantee: the tools are looked for where the daemon will look, not in the installer's own shell. A managed unit's
/// daemon gets the user manager's environment and what `~/.bashrc` exports above its non-interactive guard.
#[cfg(unix)]
#[test]
fn the_tools_are_looked_for_on_the_daemons_search_path_not_the_installers() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".bashrc"), "export PATH=\"$HOME/tools:$PATH\"\ncase $- in *i*) ;; *) return ;; esac\nexport PATH=/never\n").unwrap();
    let manager = bin_with(&dir.path().join("manager"), &["ffmpeg"]);
    let env = format!("LANG=C\nPATH={}\n", manager.to_string_lossy());
    let path = unit_path(&home, &env).expect("bash says its path");
    assert_eq!(path, OsString::from(format!("{}/tools:{}", home.display(), manager.to_string_lossy())), "the manager's path, then the bashrc's export");
    // A stub in the bashrc's folder is found there and nowhere else; the installer's own path is not the daemon's.
    let own = bin_with(&dir.path().join("own"), &["pdftoppm", "pdfinfo", "ffmpeg"]);
    assert_eq!(names(&missing(&own)), Vec::<&str>::new());
    assert_eq!(names(&missing(&path)), ["pdftoppm", "pdfinfo"], "the daemon's path lacks poppler though the installer's has it");
    bin_with(&home.join("tools"), &["pdftoppm", "pdfinfo"]);
    assert!(missing(&path).is_empty(), "the folder the bashrc adds is searched");
}

/// Guarantee: at a console the install names what is missing and what each is for, asks once with no the default, and
/// on yes runs the platform's package command for the missing ones only.
#[test]
fn a_person_at_the_console_is_told_what_is_missing_asked_once_and_no_is_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let none = bin_with(&dir.path().join("none"), &[]);
    let r = run("linux", &none, &["apt-get"], Some(""), Some(0));
    assert_eq!(r.asked.len(), 1, "asked once");
    for word in ["pdftoppm", "pdfinfo", "ffmpeg", "PDF pages", "video poster frames", "sudo apt-get install poppler-utils ffmpeg", "[y/N]"] {
        assert!(r.asked[0].contains(word), "the question names {word}: {}", r.asked[0]);
    }
    assert!(r.ran.is_empty(), "an empty answer is no: {:?}", r.ran);
    let r = run("linux", &none, &["apt-get"], Some("no"), Some(0));
    assert!(r.ran.is_empty(), "no is no");
    // Yes runs the command for what is missing: ffmpeg is there, so only poppler.
    let ffmpeg = bin_with(&dir.path().join("ffmpeg"), &["ffmpeg"]);
    let r = run("linux", &ffmpeg, &["apt-get"], Some("y"), Some(0));
    assert_eq!(r.ran, [argv(&["sudo", "apt-get", "install", "poppler-utils"])]);
    // Each platform's own package command.
    let r = run("macos", &none, &["brew"], Some("Yes"), Some(0));
    assert_eq!(r.ran, [argv(&["brew", "install", "poppler", "ffmpeg"])]);
    let r = run("windows", &none, &["winget"], Some("y"), Some(0));
    assert_eq!(r.ran, [argv(&["winget", "install", "--id", "oschwartz10612.Poppler", "-e"]), argv(&["winget", "install", "--id", "Gyan.FFmpeg", "-e"])]);
    // No package manager here: nothing is asked or run, and the one line says what to install and where from.
    let r = run("linux", &none, &[], Some("y"), Some(0));
    assert!((r.asked.is_empty(), r.ran.is_empty(), r.said.len()) == (true, true, 1), "{:?} {:?} {:?}", r.asked, r.ran, r.said);
    let r = run("windows", &none, &[], Some("y"), Some(0));
    assert!(r.ran.is_empty() && r.said.len() == 1 && r.said[0].contains("https://"), "{:?}", r.said);
}

/// Guarantee: with no person at the console the install never asks and never runs a package command: one line names the
/// missing tools and the command that would install them.
#[test]
fn with_no_person_at_the_console_one_line_names_the_command_and_nothing_runs() {
    let dir = tempfile::tempdir().unwrap();
    let none = bin_with(&dir.path().join("none"), &[]);
    for (system, has, command) in [("linux", "apt-get", "sudo apt-get install poppler-utils ffmpeg"), ("macos", "brew", "brew install poppler ffmpeg")] {
        let r = run(system, &none, &[has], None, Some(0));
        assert!(r.ran.is_empty(), "{system} ran {:?}", r.ran);
        assert_eq!(r.said.len(), 1, "{system}: one line: {:?}", r.said);
        for word in ["pdftoppm", "pdfinfo", "ffmpeg", command] {
            assert!(r.said[0].contains(word), "{system}: the line names {word}: {}", r.said[0]);
        }
    }
}

/// Guarantee: a "no", a refused password or a failed package command ends in one line of its own and nothing more; the
/// offer returns nothing, so it cannot fail the install or change its verdict.
#[test]
fn a_no_or_a_failed_package_command_is_one_line_and_never_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let none = bin_with(&dir.path().join("none"), &[]);
    for (answer, code, says) in [(Some("n"), Some(0), "sudo apt-get install"), (Some("y"), Some(1), "exit 1"), (Some("y"), None, "did not finish")] {
        let r = run("linux", &none, &["apt-get"], answer, code);
        assert_eq!(r.said.len(), 1, "{answer:?} {code:?}: one line: {:?}", r.said);
        assert!(r.said[0].contains(says), "{answer:?} {code:?}: {}", r.said[0]);
    }
    // A command that finishes but leaves a tool unfound on the daemon's path says so, as its one line.
    let r = run("linux", &none, &["apt-get"], Some("y"), Some(0));
    assert!(r.said.len() == 1 && r.said[0].contains("still not found"), "{:?}", r.said);
    // And one that leaves them all in place says it installed them.
    let all = dir.path().join("all");
    let seams = Seams {
        ask: &|_| Some("y".into()),
        run: &|_| {
            bin_with(&all, &["pdftoppm", "pdfinfo", "ffmpeg"]);
            Some(0)
        },
        say: &|_| {},
        has: &|p| p == "apt-get",
    };
    let said = RefCell::new(vec![]);
    offer("linux", all.as_os_str(), &Seams { say: &|t| said.borrow_mut().push(t.to_string()), ..seams });
    assert!(said.borrow().len() == 1 && said.borrow()[0].contains("installed"), "{:?}", said.borrow());
}

/// Guarantee: nothing new is asked, run or said when all three tools are present.
#[test]
fn nothing_is_asked_when_all_three_tools_are_present() {
    let dir = tempfile::tempdir().unwrap();
    let all = bin_with(&dir.path().join("all"), &["pdftoppm", "pdfinfo", "ffmpeg"]);
    let r = run("linux", &all, &["apt-get"], Some("y"), Some(0));
    assert!((r.asked.is_empty(), r.ran.is_empty(), r.said.is_empty()) == (true, true, true), "{:?} {:?} {:?}", r.asked, r.ran, r.said);
}
