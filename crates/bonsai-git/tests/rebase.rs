use bonsai_git::{Git, InProgress, Integration, RebaseAction, Repository};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    repository: Repository,
}

impl Fixture {
    /// `base`, then commits one to five, each adding its own file.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        let repository = Repository::init(&git(), &root, Some("main")).unwrap();
        let fixture = Self {
            _dir: dir,
            root,
            repository,
        };
        for (key, value) in [
            ("user.name", "Ada Lovelace"),
            ("user.email", "ada@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            fixture.run(&["config", key, value]);
        }
        fixture.commit("base", "base.txt");
        for n in ["one", "two", "three", "four", "five"] {
            fixture.commit(n, &format!("{n}.txt"));
        }
        fixture
    }

    fn run(&self, args: &[&str]) -> String {
        let g = git();
        g.run(&g.command(args.iter().copied()).cwd(&self.root))
            .unwrap()
            .stdout_lossy()
    }

    fn commit(&self, message: &str, file: &str) {
        std::fs::write(self.root.join(file), message).unwrap();
        self.run(&["add", "."]);
        self.run(&["commit", "-qm", message]);
    }

    fn subjects(&self) -> Vec<String> {
        self.run(&["log", "--format=%s", "--reverse"])
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[test]
fn reorders_rewords_squashes_and_drops() {
    let f = Fixture::new();
    let base = f.run(&["rev-parse", "HEAD~5"]).trim().to_owned();
    let mut steps = f.repository.rebase_steps(Some(&base)).unwrap();
    let summaries: Vec<&str> = steps.iter().map(bonsai_git::RebaseStep::summary).collect();
    assert_eq!(summaries, ["one", "two", "three", "four", "five"]);

    // five moves first; one is reworded and two folds into it; three is
    // dropped; four squashes into five with a new message.
    let five = steps.remove(4);
    steps.insert(0, five);
    steps[1].action = RebaseAction::Reword;
    steps[1].new_message = Some("First, reworded\n\nWith a body.".into());
    steps[2].action = RebaseAction::Fixup;
    steps[3].action = RebaseAction::Drop;
    steps[4].action = RebaseAction::Squash;
    // Moving four right after five makes it squash into five.
    let four = steps.remove(4);
    steps.insert(1, four);
    steps[1].new_message = Some("Five and four".into());
    std::fs::write(f.root.join("base.txt"), "uncommitted").unwrap();

    let result = f
        .repository
        .rebase_interactive(Some(&base), &steps)
        .unwrap();
    assert_eq!(result, Integration::Completed);
    assert_eq!(f.subjects(), ["base", "Five and four", "First, reworded"]);
    let body = f.run(&["log", "-1", "--format=%B"]);
    assert_eq!(body.trim_end(), "First, reworded\n\nWith a body.");
    let files = f.run(&["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(files.lines().collect::<Vec<_>>(), ["one.txt", "two.txt"]);
    assert!(!f.root.join("three.txt").exists());
    // Uncommitted changes come back.
    assert_eq!(
        std::fs::read_to_string(f.root.join("base.txt")).unwrap(),
        "uncommitted"
    );
    assert!(f.repository.in_progress().is_none());
}

#[test]
fn stops_on_conflicts_and_refuses_merges() {
    let f = Fixture::new();
    // two and three both change one.txt; swapping them conflicts.
    std::fs::write(f.root.join("one.txt"), "changed").unwrap();
    f.run(&["commit", "-qam", "six"]);
    std::fs::write(f.root.join("one.txt"), "changed again").unwrap();
    f.run(&["commit", "-qam", "seven"]);
    let base = f.run(&["rev-parse", "HEAD~2"]).trim().to_owned();
    let mut steps = f.repository.rebase_steps(Some(&base)).unwrap();
    steps.swap(0, 1);
    let result = f
        .repository
        .rebase_interactive(Some(&base), &steps)
        .unwrap();
    assert_eq!(result, Integration::Conflicts);
    assert_eq!(f.repository.in_progress(), Some(InProgress::Rebase));
    f.run(&["rebase", "--abort"]);

    f.run(&["switch", "-qc", "side", "HEAD~3"]);
    f.commit("side", "side.txt");
    f.run(&["switch", "-q", "main"]);
    f.run(&["merge", "-q", "--no-edit", "side"]);
    let base = f.run(&["rev-parse", "HEAD~4"]).trim().to_owned();
    assert!(f.repository.rebase_steps(Some(&base)).is_err());
}

#[test]
fn edits_from_the_first_commit() {
    let f = Fixture::new();
    let mut steps = f.repository.rebase_steps(None).unwrap();
    assert_eq!(steps.len(), 6);
    for step in &mut steps[1..] {
        step.action = RebaseAction::Fixup;
    }
    f.repository.rebase_interactive(None, &steps).unwrap();
    assert_eq!(f.subjects(), ["base"]);
}

#[test]
fn amends_an_earlier_commit_with_the_staged_changes() {
    let f = Fixture::new();
    let two = f.run(&["rev-parse", "HEAD~3"]).trim().to_owned();
    std::fs::write(f.root.join("two.txt"), "two, amended").unwrap();
    std::fs::write(f.root.join("five.txt"), "five, not staged").unwrap();
    f.run(&["add", "two.txt"]);

    let result = f.repository.amend_commit(&two).unwrap();

    assert_eq!(result, Integration::Completed);
    assert_eq!(
        f.subjects(),
        ["base", "one", "two", "three", "four", "five"]
    );
    assert_eq!(f.run(&["show", "HEAD~3:two.txt"]), "two, amended");
    // Unstaged work is put back as it was.
    assert_eq!(
        std::fs::read_to_string(f.root.join("five.txt")).unwrap(),
        "five, not staged"
    );
    assert_eq!(f.run(&["diff", "--cached", "--name-only"]), "");
}

#[test]
fn amends_the_last_commit_and_the_first() {
    let f = Fixture::new();
    std::fs::write(f.root.join("five.txt"), "five, amended").unwrap();
    f.run(&["add", "."]);
    f.repository.amend_commit("HEAD").unwrap();
    assert_eq!(f.run(&["show", "HEAD:five.txt"]), "five, amended");
    assert_eq!(f.subjects().len(), 6);

    let root = f
        .run(&["rev-list", "--max-parents=0", "HEAD"])
        .trim()
        .to_owned();
    std::fs::write(f.root.join("base.txt"), "base, amended").unwrap();
    f.run(&["add", "."]);
    f.repository.amend_commit(&root).unwrap();
    assert_eq!(f.run(&["show", "HEAD~5:base.txt"]), "base, amended");
    assert_eq!(
        f.subjects(),
        ["base", "one", "two", "three", "four", "five"]
    );
}

#[test]
fn refuses_to_amend_with_nothing_staged() {
    let f = Fixture::new();
    let error = f.repository.amend_commit("HEAD~2").unwrap_err();
    assert!(error.to_string().contains("Nothing is staged"), "{error}");
    assert_eq!(f.subjects().len(), 6);
}
