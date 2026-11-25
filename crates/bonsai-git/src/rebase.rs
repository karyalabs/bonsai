//! Interactive rebase: rewriting a run of commits by reordering, rewording,
//! squashing or dropping them (CONTEXT.md: Operation in progress).

use std::{ffi::OsString, fmt::Write as _, path::Path};

use crate::{Error, Integration, Oid, Repository, Result};

/// What happens to one commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebaseAction {
    /// Keep it as it is.
    Pick,
    /// Keep it with a new message.
    Reword,
    /// Combine it with the commit before, keeping both messages.
    Squash,
    /// Combine it with the commit before, keeping only that one's message.
    Fixup,
    /// Leave it out.
    Drop,
}

impl RebaseAction {
    /// Folds into the commit before it.
    #[must_use]
    pub fn joins(self) -> bool {
        matches!(self, Self::Squash | Self::Fixup)
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pick => "Pick",
            Self::Reword => "Reword",
            Self::Squash => "Squash",
            Self::Fixup => "Fixup",
            Self::Drop => "Drop",
        }
    }
}

/// One commit of an interactive rebase, in the order it will be applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebaseStep {
    pub action: RebaseAction,
    pub oid: Oid,
    /// The commit's full message as it is now.
    pub message: String,
    /// A new message: for [`RebaseAction::Reword`], or for the commit a
    /// group of squashes ends as (set on any step of the group).
    pub new_message: Option<String>,
}

impl RebaseStep {
    #[must_use]
    pub fn summary(&self) -> &str {
        self.message.lines().next().unwrap_or_default()
    }
}

/// Lines up `fixup! …` and `squash! …` commits under the commits they
/// name, as `git rebase --autosquash` does: each moves to just after its
/// target (after any earlier ones for the same target) and becomes a Fixup
/// or a Squash. A target is named by its summary or a hash prefix. Returns
/// how many commits were lined up.
pub fn autosquash(steps: &mut Vec<RebaseStep>) -> usize {
    /// The action and the target text of a `fixup!` or `squash!` summary,
    /// looking through repeats such as `fixup! fixup! x`.
    fn parse(summary: &str) -> Option<(RebaseAction, &str)> {
        let (action, mut rest) = if let Some(rest) = summary.strip_prefix("fixup! ") {
            (RebaseAction::Fixup, rest)
        } else {
            (RebaseAction::Squash, summary.strip_prefix("squash! ")?)
        };
        while let Some(inner) = rest
            .strip_prefix("fixup! ")
            .or_else(|| rest.strip_prefix("squash! "))
        {
            rest = inner;
        }
        Some((action, rest))
    }
    let mut moved = 0;
    let mut index = 0;
    while index < steps.len() {
        let Some((action, target)) = parse(steps[index].summary()) else {
            index += 1;
            continue;
        };
        let found = steps[..index].iter().position(|step| {
            parse(step.summary()).is_none()
                && (step.summary() == target
                    || (target.len() >= 4 && step.oid.as_str().starts_with(target)))
        });
        let Some(found) = found else {
            index += 1;
            continue;
        };
        // After the target and the fixups already lined up under it.
        let mut place = found + 1;
        while place < index && steps[place].action.joins() {
            place += 1;
        }
        if place != index || steps[index].action != action {
            moved += 1;
        }
        let mut step = steps.remove(index);
        step.action = action;
        steps.insert(place, step);
        index += 1;
    }
    moved
}

/// Single-quotes `value` for the shell Git runs `exec` lines and editors in.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Writes the todo list Git follows for `steps`, with message files in
/// `dir`. Rewording is an `exec` that amends the commit, run once the
/// commit's squashes are folded in, so no editor is ever needed.
///
/// # Errors
/// When the first kept commit would fold into a commit outside the rebase,
/// or the message files can't be written.
pub fn rebase_todo(git: &Path, steps: &[RebaseStep], dir: &Path) -> Result<String> {
    let io = |source| Error::Io {
        path: dir.to_path_buf(),
        source,
    };
    let mut todo = String::new();
    let mut pending: Option<String> = None;
    let mut started = false;
    let flush = |todo: &mut String, pending: &mut Option<String>| {
        if let Some(file) = pending.take() {
            let _ = writeln!(
                todo,
                "exec {} commit --amend --allow-empty --no-verify --cleanup=strip -q -F {}",
                quote(&git.to_string_lossy()),
                quote(&file)
            );
        }
    };
    for (index, step) in steps.iter().enumerate() {
        if step.action == RebaseAction::Drop {
            let _ = writeln!(todo, "drop {}", step.oid);
            continue;
        }
        if step.action.joins() {
            if !started {
                return Err(Error::Refused(format!(
                    "\u{201c}{}\u{201d} can't be squashed: no kept commit comes before it.",
                    step.summary()
                )));
            }
        } else {
            flush(&mut todo, &mut pending);
        }
        started = true;
        let word = if step.action.joins() {
            if step.action == RebaseAction::Squash {
                "squash"
            } else {
                "fixup"
            }
        } else {
            "pick"
        };
        let _ = writeln!(todo, "{word} {}", step.oid);
        if let Some(message) = &step.new_message {
            let file = dir.join(format!("message-{index}"));
            std::fs::write(&file, message).map_err(io)?;
            pending = Some(file.to_string_lossy().into_owned());
        }
    }
    flush(&mut todo, &mut pending);
    Ok(todo)
}

impl Repository {
    /// The commits after `base` up to HEAD, oldest first, as a plan that
    /// keeps every one. `None` starts from the first commit.
    ///
    /// # Errors
    /// When Git fails, or the run contains a merge, which an interactive
    /// rebase would flatten.
    pub fn rebase_steps(&self, base: Option<&str>) -> Result<Vec<RebaseStep>> {
        let range = base.map_or_else(|| "HEAD".to_owned(), |base| format!("{base}..HEAD"));
        let output = self.git().run(
            &self
                .git()
                .command([
                    "log",
                    "--reverse",
                    "--format=%H%x1f%P%x1f%B%x1e",
                    range.as_str(),
                ])
                .cwd(self.root()),
        )?;
        let text = output.stdout_lossy();
        let mut steps = Vec::new();
        for record in text.split('\u{1e}') {
            let record = record.trim_start_matches('\n');
            let mut fields = record.splitn(3, '\u{1f}');
            let (Some(oid), Some(parents), Some(message)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if parents.split(' ').filter(|p| !p.is_empty()).count() > 1 {
                return Err(Error::Refused(
                    "These commits include a merge. Editing them would flatten it, so pick a commit after the merge."
                        .to_owned(),
                ));
            }
            steps.push(RebaseStep {
                action: RebaseAction::Pick,
                oid: Oid::new(oid),
                message: message.trim_end().to_owned(),
                new_message: None,
            });
        }
        Ok(steps)
    }

    /// Rewrites the commits after `base` (`None`: from the first commit) as
    /// `steps` says, in their order. Uncommitted changes are stashed and
    /// put back. A stop with conflicts leaves the rebase in progress.
    ///
    /// # Errors
    /// When the plan is invalid or Git fails.
    pub fn rebase_interactive(
        &self,
        base: Option<&str>,
        steps: &[RebaseStep],
    ) -> Result<Integration> {
        self.rebase_interactive_as(base, steps, None)
    }

    /// [`Self::rebase_interactive`], naming `reflog_action` in the reflog.
    fn rebase_interactive_as(
        &self,
        base: Option<&str>,
        steps: &[RebaseStep],
        reflog_action: Option<&str>,
    ) -> Result<Integration> {
        if steps.iter().all(|s| s.action == RebaseAction::Drop) && base.is_none() {
            return Err(Error::Refused(
                "A branch needs at least one commit; keep one.".to_owned(),
            ));
        }
        let dir = self.git_dir().join("bonsai-rebase");
        let io = |source| Error::Io {
            path: dir.clone(),
            source,
        };
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(io)?;
        let todo = rebase_todo(self.git().program(), steps, &dir)?;
        let todo_file = dir.join("todo");
        std::fs::write(&todo_file, todo).map_err(io)?;
        let mut args: Vec<OsString> = vec!["rebase".into(), "-i".into(), "--autostash".into()];
        match base {
            Some(base) => args.push(base.into()),
            None => args.push("--root".into()),
        }
        self.integrate_with(
            args,
            &format!("cp {}", quote(&todo_file.to_string_lossy())),
            reflog_action,
        )
    }

    /// Folds the staged changes into `target`, an earlier commit on the
    /// current branch, keeping its message: a fixup commit squashed in by an
    /// interactive rebase. Unstaged changes are stashed and put back. A stop
    /// with conflicts leaves the rebase in progress.
    ///
    /// # Errors
    /// When nothing is staged, `target` isn't on the current branch, a merge
    /// follows it, or Git fails.
    pub fn amend_commit(&self, target: &str) -> Result<Integration> {
        let run = |args: &[&str]| self.git().run(&self.git().command(args).cwd(self.root()));
        let target = Oid::new(
            run(&["rev-parse", "--verify", &format!("{target}^{{commit}}")])?
                .stdout_lossy()
                .trim(),
        );
        if run(&["diff", "--cached", "--quiet"]).is_ok() {
            return Err(Error::Refused(
                "Nothing is staged. Stage the changes to add to the commit first.".to_owned(),
            ));
        }
        if run(&["merge-base", "--is-ancestor", target.as_str(), "HEAD"]).is_err() {
            return Err(Error::Refused(
                "That commit isn't on the current branch.".to_owned(),
            ));
        }
        let parent = run(&["rev-parse", "--verify", "-q", &format!("{target}^")])
            .ok()
            .map(|output| output.stdout_lossy().trim().to_owned());
        if target.as_str() == run(&["rev-parse", "HEAD"])?.stdout_lossy().trim() {
            self.commit_with(&["commit", "--amend", "--no-edit", "-q"])?;
            return Ok(Integration::Completed);
        }
        // Check before committing, so a refusal leaves nothing behind.
        let mut steps = self.rebase_steps(parent.as_deref())?;
        self.commit_with(&["commit", "-q", &format!("--fixup={target}")])?;
        let fixup = run(&["rev-parse", "HEAD"])?
            .stdout_lossy()
            .trim()
            .to_owned();
        let position = steps
            .iter()
            .position(|step| step.oid == target)
            .map_or(1, |index| index + 1);
        steps.insert(
            position,
            RebaseStep {
                action: RebaseAction::Fixup,
                oid: Oid::new(&fixup),
                message: format!("fixup! {target}"),
                new_message: None,
            },
        );
        // Named so undoing it is a soft reset that leaves the changes staged
        // again, as undoing a commit does.
        self.rebase_interactive_as(parent.as_deref(), &steps, Some(crate::undo::AMEND_ACTION))
    }

    /// Runs a commit that needs no message from an editor.
    /// Commits the staged changes as a `fixup!` of `target`, without
    /// rewriting anything yet: Edit Commits lines it up under `target`
    /// later, so a branch can collect fixes until it is ready to tidy.
    ///
    /// # Errors
    /// When nothing is staged or Git fails.
    pub fn fixup_commit(&self, target: &str) -> Result<Oid> {
        let run = |args: &[&str]| self.git().run(&self.git().command(args).cwd(self.root()));
        if run(&["diff", "--cached", "--quiet"]).is_ok() {
            return Err(Error::Refused(
                "Nothing is staged. Stage the changes for the fixup first.".to_owned(),
            ));
        }
        self.commit_with(&["commit", "-q", &format!("--fixup={target}")])?;
        Ok(Oid::new(run(&["rev-parse", "HEAD"])?.stdout_lossy().trim()))
    }

    fn commit_with(&self, args: &[&str]) -> Result<()> {
        self.git()
            .run(
                &self
                    .git()
                    .command(args)
                    .cwd(self.root())
                    .env("GIT_EDITOR", "true"),
            )
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titled(oid: &str, summary: &str) -> RebaseStep {
        RebaseStep {
            action: RebaseAction::Pick,
            oid: Oid::new(oid),
            message: summary.to_owned(),
            new_message: None,
        }
    }

    #[test]
    fn autosquash_lines_fixups_up_under_their_targets() {
        let mut steps = vec![
            titled("aaaa1111", "Add login"),
            titled("bbbb2222", "Add logout"),
            titled("cccc3333", "fixup! Add login"),
            titled("dddd4444", "squash! aaaa1111"),
            titled("eeee5555", "fixup! fixup! Add logout"),
            titled("ffff6666", "fixup! Something not here"),
        ];
        assert_eq!(autosquash(&mut steps), 3);
        let order: Vec<(&str, RebaseAction)> =
            steps.iter().map(|s| (s.oid.as_str(), s.action)).collect();
        assert_eq!(
            order,
            [
                ("aaaa1111", RebaseAction::Pick),
                ("cccc3333", RebaseAction::Fixup),
                ("dddd4444", RebaseAction::Squash),
                ("bbbb2222", RebaseAction::Pick),
                ("eeee5555", RebaseAction::Fixup),
                ("ffff6666", RebaseAction::Pick),
            ]
        );
        assert_eq!(autosquash(&mut steps), 0, "already lined up stays put");
    }

    fn step(action: RebaseAction, oid: &str, new_message: Option<&str>) -> RebaseStep {
        RebaseStep {
            action,
            oid: Oid::new(oid),
            message: format!("commit {oid}"),
            new_message: new_message.map(str::to_owned),
        }
    }

    #[test]
    fn writes_rewords_after_their_squashes() {
        let dir = tempfile::tempdir().unwrap();
        let steps = [
            step(RebaseAction::Reword, "a", Some("New A")),
            step(RebaseAction::Fixup, "b", None),
            step(RebaseAction::Drop, "c", None),
            step(RebaseAction::Pick, "d", None),
            step(RebaseAction::Squash, "e", Some("D and E")),
        ];
        let todo = rebase_todo(Path::new("/usr/bin/git"), &steps, dir.path()).unwrap();
        let lines: Vec<&str> = todo.lines().collect();
        assert_eq!(lines[0], "pick a");
        assert_eq!(lines[1], "fixup b");
        // Dropping changes nothing, so the reword can follow it.
        assert_eq!(lines[2], "drop c");
        assert!(lines[3].starts_with("exec '/usr/bin/git' commit --amend"));
        assert!(lines[3].ends_with("message-0'"));
        assert_eq!(lines[4], "pick d");
        assert_eq!(lines[5], "squash e");
        assert!(lines[6].ends_with("message-4'"));
        assert_eq!(lines.len(), 7);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("message-0")).unwrap(),
            "New A"
        );
    }

    #[test]
    fn refuses_to_squash_the_first_commit() {
        let dir = tempfile::tempdir().unwrap();
        let steps = [
            step(RebaseAction::Drop, "a", None),
            step(RebaseAction::Squash, "b", None),
        ];
        assert!(rebase_todo(Path::new("git"), &steps, dir.path()).is_err());
    }

    #[test]
    fn quotes_for_the_shell() {
        assert_eq!(quote("it's"), r"'it'\''s'");
    }
}
