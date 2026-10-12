//! Pure decisions for which board a pane opens (spec section 5, "Opening a board").

use crate::model::{BoardRef, ProjectSummary, RepoSlug};

#[derive(Debug, Clone, PartialEq)]
pub enum FirstStep {
    Use(BoardRef),
    FetchRepoProjects(RepoSlug),
    PickAll,
}

/// `--project` wins; `--picker` always picks; then the board remembered for the repo; then
/// the repo's linked boards; with no repo, the picker.
pub fn first_step(
    explicit: Option<BoardRef>,
    picker: bool,
    remembered: Option<BoardRef>,
    repo: Option<RepoSlug>,
) -> FirstStep {
    if let Some(b) = explicit {
        return FirstStep::Use(b);
    }
    if picker {
        return FirstStep::PickAll;
    }
    if let Some(b) = remembered {
        return FirstStep::Use(b);
    }
    match repo {
        Some(r) => FirstStep::FetchRepoProjects(r),
        None => FirstStep::PickAll,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AfterRepo {
    Use(BoardRef),
    Pick(Vec<ProjectSummary>),
    PickAll,
}

/// One open linked board opens directly; several are picked from; none falls back to all boards.
pub fn after_repo_projects(found: Vec<ProjectSummary>) -> AfterRepo {
    let open: Vec<_> = found.into_iter().filter(|p| !p.closed).collect();
    match open.len() {
        0 => AfterRepo::PickAll,
        1 => AfterRepo::Use(open[0].board.clone()),
        _ => AfterRepo::Pick(open),
    }
}

/// Open boards only, the repo's linked boards first, each board once.
pub fn picker_candidates(
    all: Vec<ProjectSummary>,
    linked_first: &[ProjectSummary],
) -> Vec<ProjectSummary> {
    let mut out: Vec<ProjectSummary> = Vec::new();
    for p in linked_first.iter().cloned().chain(all) {
        if !p.closed && !out.iter().any(|o| o.board == p.board) {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProjectId;

    fn summary(board: &str, closed: bool) -> ProjectSummary {
        ProjectSummary {
            id: ProjectId::new(board),
            board: board.parse().unwrap(),
            title: board.into(),
            closed,
        }
    }
    fn b(s: &str) -> BoardRef {
        s.parse().unwrap()
    }
    fn r() -> RepoSlug {
        "tviles/app".parse().unwrap()
    }

    #[test]
    fn first_step_order() {
        assert_eq!(
            first_step(Some(b("a/1")), true, Some(b("a/2")), Some(r())),
            FirstStep::Use(b("a/1"))
        );
        assert_eq!(
            first_step(None, true, Some(b("a/2")), Some(r())),
            FirstStep::PickAll
        );
        assert_eq!(
            first_step(None, false, Some(b("a/2")), Some(r())),
            FirstStep::Use(b("a/2"))
        );
        assert_eq!(
            first_step(None, false, None, Some(r())),
            FirstStep::FetchRepoProjects(r())
        );
        assert_eq!(first_step(None, false, None, None), FirstStep::PickAll);
    }

    #[test]
    fn after_repo_ignores_closed_boards() {
        assert_eq!(after_repo_projects(vec![]), AfterRepo::PickAll);
        assert_eq!(
            after_repo_projects(vec![summary("a/1", true)]),
            AfterRepo::PickAll
        );
        assert_eq!(
            after_repo_projects(vec![summary("a/1", false), summary("a/2", true)]),
            AfterRepo::Use(b("a/1"))
        );
        assert!(matches!(
            after_repo_projects(vec![summary("a/1", false), summary("a/2", false)]),
            AfterRepo::Pick(v) if v.len() == 2
        ));
    }

    #[test]
    fn candidates_put_linked_first_without_duplicates() {
        let all = vec![
            summary("a/1", false),
            summary("a/2", false),
            summary("a/3", true),
        ];
        let linked = vec![summary("a/2", false)];
        let boards: Vec<_> = picker_candidates(all, &linked)
            .into_iter()
            .map(|p| p.board.to_string())
            .collect();
        assert_eq!(boards, ["a/2", "a/1"]);
    }
}
