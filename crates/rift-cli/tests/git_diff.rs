//! `rift git-diff` as a `diff.<driver>.command` of a real repository: git's
//! own headers, exit codes, and Git LFS pointers read from the local store.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_rift");

fn git_available() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

/// Returns early with a message when `git` is not on PATH.
macro_rules! need_git {
    () => {
        if !git_available() {
            eprintln!("skipped: git is not on PATH");
            return;
        }
    };
}

struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    /// A repository whose `*.txt` and `*.dat` files go through the driver.
    fn new(driver_args: &str) -> Self {
        let repo = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "t@example.com"]);
        repo.git(&["config", "user.name", "t"]);
        repo.git(&[
            "config",
            "diff.rift.command",
            &format!("'{BIN}' git-diff {driver_args}"),
        ]);
        repo.write(".gitattributes", "*.txt diff=rift\n*.dat diff=rift\n");
        repo
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, name: &str, data: impl AsRef<[u8]>) {
        let p = self.path().join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }

    fn git(&self, args: &[&str]) -> Output {
        run_git(self.path(), args)
    }

    fn commit(&self, msg: &str) {
        self.git(&["add", "-A"]);
        let out = self.git(&["commit", "-q", "-m", msg]);
        assert!(out.status.success(), "{}", lossy(&out.stderr));
    }

    /// The oid git gives the blob at `rev:path`.
    fn lfs_store(&self, content: &[u8]) -> String {
        store_object(&self.path().join(".git"), content)
    }
}

fn run_git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_EXTERNAL_DIFF")
        .output()
        .unwrap()
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A stand-in for SHA-256: git-lfs names objects by hash, but rift only
/// uses the name to find the file.
fn fake_oid(content: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in content {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}").repeat(4)
}

fn pointer(content: &[u8]) -> String {
    format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
        fake_oid(content),
        content.len()
    )
}

fn store_object(git_common_dir: &Path, content: &[u8]) -> String {
    let oid = fake_oid(content);
    let dir = git_common_dir
        .join("lfs/objects")
        .join(&oid[0..2])
        .join(&oid[2..4]);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(&oid), content).unwrap();
    oid
}

fn lines(n: usize, changed: Option<usize>) -> String {
    (0..n)
        .map(|i| {
            if Some(i) == changed {
                format!("{i} changed\n")
            } else {
                format!("{i} row\n")
            }
        })
        .collect()
}

/// `git diff <args>` through the driver and without it.
fn both(repo: &Repo, args: &[&str]) -> (Output, Output) {
    let mut ext = vec!["diff", "--ext-diff"];
    ext.extend(args);
    let mut plain = vec!["diff", "--no-ext-diff"];
    plain.extend(args);
    (repo.git(&ext), repo.git(&plain))
}

#[test]
fn headers_match_git_for_modify_add_delete_mode_and_rename() {
    need_git!();
    let repo = Repo::new("");
    repo.write("m.txt", lines(40, None));
    repo.write("gone.txt", lines(10, None));
    repo.write("mode.txt", lines(10, None));
    repo.write("old name.txt", lines(60, None));
    repo.commit("base");
    repo.write("m.txt", lines(40, Some(20)));
    repo.write("added.txt", "new\nfile\n");
    repo.write("empty.txt", "");
    fs::remove_file(repo.path().join("gone.txt")).unwrap();
    repo.write("mode.txt", lines(10, Some(2)));
    repo.git(&["mv", "old name.txt", "same.txt"]);
    repo.git(&["add", "-A"]);
    repo.git(&["update-index", "--chmod=+x", "mode.txt"]);
    // Staged, so git has every blob hash that the driver is given.
    let (ext, plain) = both(&repo, &["--cached", "-M"]);
    assert!(ext.status.success(), "{}", lossy(&ext.stderr));
    assert_eq!(lossy(&ext.stdout), lossy(&plain.stdout));
    assert!(lossy(&ext.stdout).contains("rename from old name.txt"));
    assert!(lossy(&ext.stdout).contains("new file mode 100644"));
    assert!(lossy(&ext.stdout).contains("deleted file mode 100644"));
    assert!(lossy(&ext.stdout).contains("new mode 100755"));
}

#[test]
fn a_rename_with_edits_gets_git_headers_and_hunks() {
    need_git!();
    let repo = Repo::new("");
    repo.write("a.txt", lines(60, None));
    repo.commit("base");
    repo.git(&["mv", "a.txt", "b.txt"]);
    repo.write("b.txt", lines(60, Some(30)));
    repo.git(&["add", "-A"]);
    let (ext, plain) = both(&repo, &["--cached", "-M"]);
    assert!(ext.status.success(), "{}", lossy(&ext.stderr));
    assert_eq!(lossy(&ext.stdout), lossy(&plain.stdout));
    assert!(lossy(&ext.stdout).contains("rename to b.txt\n"));
}

#[test]
fn the_work_tree_side_shows_a_null_hash() {
    need_git!();
    let repo = Repo::new("");
    repo.write("m.txt", lines(40, None));
    repo.commit("base");
    repo.write("m.txt", lines(40, Some(5)));
    let (ext, plain) = both(&repo, &[]);
    let (ext, plain) = (lossy(&ext.stdout), lossy(&plain.stdout));
    // git hashes the file for its own diff; the driver is not given that.
    let strip = |s: &str| {
        s.lines()
            .filter(|l| !l.starts_with("index "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(strip(&ext), strip(&plain));
    let index = ext.lines().find(|l| l.starts_with("index ")).unwrap();
    assert!(index.ends_with("..0000000 100644"), "{index}");
}

#[test]
fn git_sees_exit_zero_on_differences_and_still_reports_them() {
    need_git!();
    let repo = Repo::new("--mem 64M");
    repo.write("m.txt", lines(10, None));
    repo.commit("base");
    repo.write("m.txt", lines(10, Some(3)));
    let out = repo.git(&["diff", "--ext-diff"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(lossy(&out.stdout).contains("+3 changed"));
    let out = repo.git(&["diff", "--ext-diff", "--exit-code"]);
    assert_eq!(out.status.code(), Some(1), "git's own status for a diff");
    repo.git(&["checkout", "m.txt"]);
    let out = repo.git(&["diff", "--ext-diff", "--exit-code"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(lossy(&out.stdout), "");
}

#[test]
fn a_budget_too_small_fails_the_git_diff() {
    need_git!();
    let repo = Repo::new("--mem 1K");
    repo.write("m.txt", lines(5000, None));
    repo.commit("base");
    repo.write("m.txt", lines(5000, Some(100)));
    let out = repo.git(&["diff", "--ext-diff"]);
    assert_eq!(out.status.code(), Some(128));
    assert!(
        lossy(&out.stderr).contains("--mem"),
        "{}",
        lossy(&out.stderr)
    );
}

#[test]
fn lfs_pointers_are_resolved_to_the_local_objects() {
    need_git!();
    let repo = Repo::new("");
    let (v1, v2) = (lines(3000, None), lines(3000, Some(1500)));
    repo.write("big.dat", pointer(v1.as_bytes()));
    repo.commit("v1");
    repo.write("big.dat", pointer(v2.as_bytes()));
    repo.commit("v2");
    repo.lfs_store(v1.as_bytes());
    repo.lfs_store(v2.as_bytes());
    let out = repo.git(&["diff", "--ext-diff", "HEAD~1", "HEAD"]);
    assert_eq!(out.status.code(), Some(0), "{}", lossy(&out.stderr));
    let text = lossy(&out.stdout);
    assert!(
        text.starts_with("diff --git a/big.dat b/big.dat\nindex "),
        "{text}"
    );
    assert!(text.contains("\n--- a/big.dat\n+++ b/big.dat\n@@ -1498,7 +1498,7 @@"));
    assert!(text.contains("-1500 row\n+1500 changed\n"));
    assert!(!text.contains("git-lfs"), "the pointer text is not diffed");
}

#[test]
fn a_pointer_against_a_work_tree_file_and_added_pointers_work() {
    need_git!();
    let repo = Repo::new("");
    let v1 = lines(100, None);
    repo.write("big.dat", pointer(v1.as_bytes()));
    repo.commit("v1");
    repo.lfs_store(v1.as_bytes());
    // A smudged work tree file: the real content, with one line changed.
    repo.write("big.dat", lines(100, Some(7)));
    let out = repo.git(&["diff", "--ext-diff"]);
    assert_eq!(out.status.code(), Some(0), "{}", lossy(&out.stderr));
    assert!(lossy(&out.stdout).contains("-7 row\n+7 changed\n"));

    let new = lines(5, None);
    repo.write("new.dat", pointer(new.as_bytes()));
    repo.lfs_store(new.as_bytes());
    repo.git(&["add", "new.dat"]);
    let out = repo.git(&["diff", "--ext-diff", "--cached"]);
    assert_eq!(out.status.code(), Some(0), "{}", lossy(&out.stderr));
    let text = lossy(&out.stdout);
    assert!(text.contains("new file mode 100644\n"), "{text}");
    assert!(text.contains("--- /dev/null\n+++ b/new.dat\n@@ -0,0 +1,5 @@\n+0 row\n"));
}

#[test]
fn a_missing_lfs_object_names_the_oid_and_suggests_lfs_fetch() {
    need_git!();
    let repo = Repo::new("");
    let (v1, v2) = (lines(50, None), lines(50, Some(3)));
    repo.write("big.dat", pointer(v1.as_bytes()));
    repo.commit("v1");
    repo.write("big.dat", pointer(v2.as_bytes()));
    repo.commit("v2");
    let out = repo.git(&["diff", "--ext-diff", "HEAD~1", "HEAD"]);
    assert_eq!(out.status.code(), Some(128));
    let err = lossy(&out.stderr);
    assert!(err.contains(&fake_oid(v1.as_bytes())), "{err}");
    assert!(err.contains("git lfs fetch"), "{err}");
}

#[test]
fn an_lfs_object_of_the_wrong_size_is_refused() {
    need_git!();
    let repo = Repo::new("");
    let (v1, v2) = (lines(50, None), lines(50, Some(3)));
    repo.write("big.dat", pointer(v1.as_bytes()));
    repo.commit("v1");
    repo.write("big.dat", pointer(v2.as_bytes()));
    repo.commit("v2");
    repo.lfs_store(v2.as_bytes());
    // v1's object is there but truncated.
    let oid = repo.lfs_store(v1.as_bytes());
    let p = repo.path().join(format!(
        ".git/lfs/objects/{}/{}/{oid}",
        &oid[0..2],
        &oid[2..4]
    ));
    fs::write(p, &v1.as_bytes()[..10]).unwrap();
    let out = repo.git(&["diff", "--ext-diff", "HEAD~1", "HEAD"]);
    assert_eq!(out.status.code(), Some(128));
    let err = lossy(&out.stderr);
    assert!(err.contains("10 bytes") && err.contains(&oid), "{err}");
}

#[test]
fn a_linked_work_tree_finds_the_objects_in_the_common_git_dir() {
    need_git!();
    let repo = Repo::new("");
    let (v1, v2) = (lines(50, None), lines(50, Some(3)));
    repo.write("big.dat", pointer(v1.as_bytes()));
    repo.commit("v1");
    repo.write("big.dat", pointer(v2.as_bytes()));
    repo.commit("v2");
    repo.lfs_store(v1.as_bytes());
    repo.lfs_store(v2.as_bytes());
    let linked: PathBuf = repo.path().join("linked");
    let out = repo.git(&["worktree", "add", "-q", "linked", "-b", "other"]);
    assert!(out.status.success(), "{}", lossy(&out.stderr));
    let out = run_git(&linked, &["diff", "--ext-diff", "HEAD~1", "HEAD"]);
    assert_eq!(out.status.code(), Some(0), "{}", lossy(&out.stderr));
    assert!(lossy(&out.stdout).contains("-3 row\n+3 changed\n"));
}

#[test]
fn run_by_hand_it_exits_two_on_errors() {
    for args in [
        &["git-diff"][..],
        &["git-diff", "a", "b", "c"],
        &["git-diff", "1", "2", "3", "4", "5", "6", "7", "8"],
    ] {
        let out = Command::new(BIN).args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(lossy(&out.stderr).starts_with("rift: "), "{args:?}");
    }
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .current_dir(dir.path())
        .args([
            "git-diff", "f", "nope", "1", "100644", "nope2", "2", "100644",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(lossy(&out.stderr).contains("nope"));
}

#[test]
fn identical_files_print_nothing_and_exit_zero() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("x"), "same\n").unwrap();
    fs::write(dir.path().join("y"), "same\n").unwrap();
    let out = Command::new(BIN)
        .current_dir(dir.path())
        .args([
            "git-diff", "f", "x", "1111111", "100644", "y", "1111111", "100644",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(lossy(&out.stdout), "diff --git a/f b/f\n");
}
