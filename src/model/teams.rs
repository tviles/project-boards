//! Team-owned values on Linear boards (spec section 6): workflow states, cycles and the
//! labels of team-level label groups belong to one team, and Linear refuses them on another
//! team's issues. Every path that sets a value on a Linear issue asks this resolver for the
//! value of that issue's own team. On a GitHub board every value passes through unchanged.

use crate::model::field::{Bucket, Field, FieldKind, OptionKind, SelectOption};
use crate::model::item::{FieldValue, Item};
use crate::model::project::{LinearMeta, Project, TeamMeta};

/// The id of a Linear board's Status field. `model` cannot see `linear`, so `linear::ids`
/// takes its value from here.
pub const STATUS_FIELD: &str = "linear:status";
/// The id of a Linear board's Cycle field.
pub const CYCLE_FIELD: &str = "linear:cycle";
/// What the field id of a single-select label group starts with; the group's label id
/// follows.
pub const GROUP_FIELD_PREFIX: &str = "linear:group:";

/// Why a value cannot be set on an issue of a team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamRefusal {
    /// The team has no workflow state of that name.
    NoState {
        team: String,
        state: String,
    },
    /// The cycle belongs to `owner`, not to `team`.
    OtherTeamsCycle {
        team: String,
        owner: String,
    },
    /// The label group `group` (its name) belongs to the team `owner` (its key).
    OtherTeamsGroup {
        group: String,
        owner: String,
    },
    /// States of kind `duplicate` are never a target.
    Duplicate,
    StatusRequired,
    /// The team has no state of `kinds` (`completed`, `canceled` or `unstarted or backlog`).
    NoStateOfKind {
        team: String,
        kinds: &'static str,
    },
    /// The board was cached before it knew its teams (by 0.4), or a cycle started after the
    /// board was resolved.
    NeedsRefresh,
    /// The board knows its teams, and the issue's team (its key) is not one of them.
    TeamNotOnBoard {
        team: String,
    },
}

impl TeamRefusal {
    /// The one-line reason shown for the refusal.
    pub fn message(&self) -> String {
        match self {
            Self::NoState { team, state } => format!("{team} has no \"{state}\" state"),
            Self::OtherTeamsCycle { team, owner } => {
                format!("{team} issues can't be in {owner}'s cycles")
            }
            Self::OtherTeamsGroup { group, owner } => {
                format!("\"{group}\" belongs to team {owner}")
            }
            Self::Duplicate => "Mark duplicates in Linear".into(),
            Self::StatusRequired => "Every Linear issue has a status".into(),
            Self::NoStateOfKind { team, kinds } => format!("{team} has no {kinds} state"),
            Self::NeedsRefresh => crate::model::Denial::NeedsRefresh.message(&|_| None),
            Self::TeamNotOnBoard { team } => format!("{team} is not one of this board's teams"),
        }
    }
}

/// Which kind of team-owned field `field` is on a Linear board.
enum Owned<'f> {
    Status,
    Cycle,
    /// A single-select label group, by its label id.
    Group(&'f str),
}

fn owned(field: &Field) -> Option<Owned<'_>> {
    let id = field.id.as_str();
    match id {
        STATUS_FIELD => Some(Owned::Status),
        CYCLE_FIELD => Some(Owned::Cycle),
        _ => id.strip_prefix(GROUP_FIELD_PREFIX).map(Owned::Group),
    }
}

fn options(field: &Field) -> &[SelectOption] {
    match &field.kind {
        FieldKind::SingleSelect { options } => options,
        _ => &[],
    }
}

/// The key of the team `id`; the id itself for a team the board does not list.
fn team_key(meta: &LinearMeta, id: &str) -> String {
    meta.teams
        .iter()
        .find(|t| t.id == id)
        .map_or_else(|| id.to_string(), |t| t.key.clone())
}

/// The team of `item`, by its team key; `None` on GitHub boards and for a team the board
/// does not know.
pub fn issue_team<'a>(project: &'a Project, item: &Item) -> Option<&'a TeamMeta> {
    let key = &item.reference()?.container;
    project
        .linear
        .as_ref()?
        .teams
        .iter()
        .find(|t| &t.key == key)
}

/// Why `item`'s team cannot be used: a board that knows no teams (cached by 0.4) needs a
/// refresh; one that knows them says the issue's team is not one of them.
pub fn unknown_team(project: &Project, item: &Item) -> TeamRefusal {
    let knows_teams = project.linear.as_ref().is_some_and(|m| !m.teams.is_empty());
    match item.reference() {
        Some(r) if knows_teams => TeamRefusal::TeamNotOnBoard {
            team: r.container.clone(),
        },
        _ => TeamRefusal::NeedsRefresh,
    }
}

/// `value` for an issue of `team` in `field`. Status: the team's state of the value's name.
/// Cycle: only the team's own cycles. A team-level label group: only on its team's issues.
/// Every other field, and a value of another shape, is unchanged.
pub fn for_team(
    project: &Project,
    team: &TeamMeta,
    field: &Field,
    value: FieldValue,
) -> Result<FieldValue, TeamRefusal> {
    let Some(meta) = &project.linear else {
        return Ok(value);
    };
    let owner = |id: &str| meta.owners.get(id).map(String::as_str);
    match owned(field) {
        Some(Owned::Status) => {
            let FieldValue::SingleSelect { option_id, name } = &value else {
                return Ok(value);
            };
            let options = options(field);
            let chosen = options.iter().find(|o| &o.id == option_id);
            let name = chosen.map_or(name.as_str(), |o| o.name.as_str());
            let is_duplicate = |o: &SelectOption| o.kind == Some(OptionKind::Duplicate);
            if chosen.is_some_and(is_duplicate) {
                return Err(TeamRefusal::Duplicate);
            }
            match options
                .iter()
                .find(|o| o.name == name && owner(o.id.as_str()) == Some(team.id.as_str()))
            {
                Some(o) if is_duplicate(o) => Err(TeamRefusal::Duplicate),
                Some(o) => Ok(FieldValue::SingleSelect {
                    option_id: o.id.clone(),
                    name: o.name.clone(),
                }),
                None => Err(TeamRefusal::NoState {
                    team: team.key.clone(),
                    state: name.to_string(),
                }),
            }
        }
        Some(Owned::Cycle) => {
            let FieldValue::Iteration { iteration_id, .. } = &value else {
                return Ok(value);
            };
            match owner(iteration_id.as_str()) {
                Some(id) if id == team.id => Ok(value),
                Some(id) => Err(TeamRefusal::OtherTeamsCycle {
                    team: team.key.clone(),
                    owner: team_key(meta, id),
                }),
                // A cycle that started after the board was resolved.
                None => Err(TeamRefusal::NeedsRefresh),
            }
        }
        Some(Owned::Group(group)) => match owner(group) {
            Some(id) if id != team.id => Err(TeamRefusal::OtherTeamsGroup {
                group: field.name.clone(),
                owner: team_key(meta, id),
            }),
            _ => Ok(value),
        },
        None => Ok(value),
    }
}

/// `value` for `item` in `field`: unchanged on GitHub boards; else its team's value
/// (`for_team`). A team-owned field of an issue whose team the board does not know is
/// refused (`unknown_team`), as is every label group while the board knows no teams (cached
/// by 0.4).
pub fn for_issue(
    project: &Project,
    item: &Item,
    field: &Field,
    value: FieldValue,
) -> Result<FieldValue, TeamRefusal> {
    let Some(meta) = &project.linear else {
        return Ok(value);
    };
    if let Some(team) = issue_team(project, item) {
        return for_team(project, team, field, value);
    }
    let team_owned = match owned(field) {
        Some(Owned::Status | Owned::Cycle) => true,
        Some(Owned::Group(group)) => meta.teams.is_empty() || meta.owners.contains_key(group),
        None => false,
    };
    if team_owned {
        Err(unknown_team(project, item))
    } else {
        Ok(value)
    }
}

/// Whether `field` can be cleared: every Linear issue has a status.
pub fn may_clear(project: &Project, field: &Field) -> Result<(), TeamRefusal> {
    if project.linear.is_some() && field.id.as_str() == STATUS_FIELD {
        Err(TeamRefusal::StatusRequired)
    } else {
        Ok(())
    }
}

/// Where closing or reopening an issue moves it (spec section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateGoal {
    Completed,
    Canceled,
    Reopen,
}

/// The Status value that closes or reopens an issue of `team`: the team's first state of
/// the goal's kind in the Status field's option order (rank, then position). Reopening
/// tries `unstarted`, then `backlog`.
pub fn state_for(
    project: &Project,
    team: &TeamMeta,
    goal: StateGoal,
) -> Result<FieldValue, TeamRefusal> {
    use OptionKind::*;
    let (kinds, label): (&[OptionKind], &'static str) = match goal {
        StateGoal::Completed => (&[Completed], "completed"),
        StateGoal::Canceled => (&[Canceled], "canceled"),
        StateGoal::Reopen => (&[Unstarted, Backlog], "unstarted or backlog"),
    };
    let options = project
        .fields
        .iter()
        .find(|f| f.id.as_str() == STATUS_FIELD)
        .map_or(&[][..], options);
    let owners = project.linear.as_ref().map(|m| &m.owners);
    let own =
        |o: &&SelectOption| owners.and_then(|owners| owners.get(o.id.as_str())) == Some(&team.id);
    kinds
        .iter()
        .find_map(|kind| options.iter().filter(own).find(|o| o.kind == Some(*kind)))
        .map(|o| FieldValue::SingleSelect {
            option_id: o.id.clone(),
            name: o.name.clone(),
        })
        .ok_or_else(|| TeamRefusal::NoStateOfKind {
            team: team.key.clone(),
            kinds: label,
        })
}

/// The one team owning every option behind `bucket` (its key and aliases), else `None`: a
/// merged column, the "No" column, and a field no team owns.
pub fn column_team<'a>(
    project: &'a Project,
    field: &Field,
    bucket: &Bucket,
) -> Option<&'a TeamMeta> {
    let meta = project.linear.as_ref()?;
    let owned = owned(field)?;
    let key = bucket.key.as_deref()?;
    let team = match owned {
        // A group's labels are not in `owners`: the group is, whatever the bucket.
        Owned::Group(group) => meta.owners.get(group)?,
        Owned::Status | Owned::Cycle => {
            let team = meta.owners.get(key)?;
            bucket
                .aliases
                .iter()
                .all(|a| meta.owners.get(a) == Some(team))
                .then_some(team)?
        }
    };
    meta.teams.iter().find(|t| &t.id == team)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::field::{Iteration, OptionColor};
    use crate::model::ids::{FieldId, IterationId, OptionId, ProjectId};
    use crate::model::item::{ContentRef, ItemContent};
    use crate::model::project::OwnerKind;

    const TBA_ID: &str = "team-a";
    const TBB_ID: &str = "team-b";
    const PRIORITY: &str = "linear:priority";

    fn tba() -> TeamMeta {
        TeamMeta {
            id: TBA_ID.into(),
            key: "TBA".into(),
        }
    }

    fn tbb() -> TeamMeta {
        TeamMeta {
            id: TBB_ID.into(),
            key: "TBB".into(),
        }
    }

    fn opt(id: &str, name: &str, kind: Option<OptionKind>) -> SelectOption {
        SelectOption {
            id: OptionId::new(id),
            name: name.into(),
            color: OptionColor::Gray,
            kind,
        }
    }

    fn select(id: &str, name: &str) -> FieldValue {
        FieldValue::SingleSelect {
            option_id: OptionId::new(id),
            name: name.into(),
        }
    }

    fn select_field(id: &str, name: &str, options: Vec<SelectOption>) -> Field {
        Field {
            id: FieldId::new(id),
            name: name.into(),
            kind: FieldKind::SingleSelect { options },
        }
    }

    fn group_field(group: &str, name: &str, options: Vec<SelectOption>) -> Field {
        select_field(&format!("{GROUP_FIELD_PREFIX}{group}"), name, options)
    }

    /// Teams TBA and TBB. TBA: `Todo`, `In Progress`, `Done`, `Canceled`, and the cycles;
    /// TBB: `Todo`, `In Review`, `Done`, `Dup` (duplicate). Group `Type` is TBA's, group
    /// `Size` the workspace's. Status options are in rank, then position, order.
    fn project() -> Project {
        use OptionKind::*;
        let status = select_field(
            STATUS_FIELD,
            "Status",
            vec![
                opt("s-a-todo", "Todo", Some(Unstarted)),
                opt("s-b-todo", "Todo", Some(Unstarted)),
                opt("s-a-prog", "In Progress", Some(Started)),
                opt("s-b-review", "In Review", Some(Started)),
                opt("s-a-done", "Done", Some(Completed)),
                opt("s-b-done", "Done", Some(Completed)),
                opt("s-a-canceled", "Canceled", Some(Canceled)),
                opt("s-b-dup", "Dup", Some(Duplicate)),
            ],
        );
        let priority = select_field(
            PRIORITY,
            "Priority",
            vec![opt("linear:priority:1", "Urgent", None)],
        );
        let iteration = |id: &str, start: &str| Iteration {
            id: IterationId::new(id),
            title: id.into(),
            start_date: start.into(),
            duration_days: 14,
        };
        let cycle = Field {
            id: FieldId::new(CYCLE_FIELD),
            name: "Cycle".into(),
            kind: FieldKind::Iteration {
                iterations: vec![iteration("c-now", "2026-10-12")],
                completed: vec![iteration("c-past", "2026-09-28")],
            },
        };
        let type_group = group_field(
            "lg-type",
            "Type",
            vec![opt("l-bug", "Bug", None), opt("l-feature", "Feature", None)],
        );
        let size_group = group_field("lg-size", "Size", vec![opt("l-small", "Small", None)]);
        let owners = [
            ("s-a-todo", TBA_ID),
            ("s-a-prog", TBA_ID),
            ("s-a-done", TBA_ID),
            ("s-a-canceled", TBA_ID),
            ("s-b-todo", TBB_ID),
            ("s-b-review", TBB_ID),
            ("s-b-done", TBB_ID),
            ("s-b-dup", TBB_ID),
            ("c-now", TBA_ID),
            ("c-past", TBA_ID),
            ("lg-type", TBA_ID),
        ]
        .into_iter()
        .map(|(id, team)| (id.to_string(), team.to_string()))
        .collect();
        Project {
            id: ProjectId::new("linear:project:p1"),
            board: "linear:project:p1".parse().unwrap(),
            owner_kind: OwnerKind::Organization,
            title: "Launch".into(),
            url: "https://linear.app/acme/project/launch".into(),
            viewer_can_update: true,
            fields: vec![status, priority, type_group, size_group, cycle],
            views: vec![],
            linked_repos: vec![],
            issue_repos: vec![],
            linear: Some(LinearMeta {
                url_key: "acme".into(),
                viewer_id: "u-1".into(),
                team_keys: vec!["TBA".into(), "TBB".into()],
                teams: vec![tba(), tbb()],
                owners,
            }),
        }
    }

    fn field<'a>(p: &'a Project, id: &str) -> &'a Field {
        p.field(&FieldId::new(id))
            .unwrap_or_else(|| panic!("no field {id}"))
    }

    fn status(p: &Project) -> &Field {
        field(p, STATUS_FIELD)
    }

    fn group<'a>(p: &'a Project, group: &str) -> &'a Field {
        field(p, &format!("{GROUP_FIELD_PREFIX}{group}"))
    }

    /// The Linear issue `<team>-<number>`.
    fn issue(team: &str, number: u32) -> Item {
        let mut item = crate::model::item::tests::issue(&format!("i-{number}"), number, "x");
        item.content = ItemContent::Issue {
            reference: ContentRef {
                container: team.into(),
                number,
                key: Some(format!("{team}-{number}")),
                url: format!("https://linear.app/acme/issue/{team}-{number}"),
            },
            title: "x".into(),
            state: crate::model::item::ContentState::Open,
        };
        item
    }

    fn cycle(id: &str) -> FieldValue {
        FieldValue::Iteration {
            iteration_id: IterationId::new(id),
            title: id.into(),
            start_date: "2026-10-12".into(),
        }
    }

    fn refusal(r: Result<FieldValue, TeamRefusal>) -> String {
        r.expect_err("refused").message()
    }

    #[test]
    fn issues_find_their_team_by_key() {
        let p = project();
        assert_eq!(issue_team(&p, &issue("TBA", 1)), Some(&tba()));
        assert_eq!(issue_team(&p, &issue("TBB", 2)), Some(&tbb()));
        assert_eq!(issue_team(&p, &issue("TBC", 3)), None);
    }

    #[test]
    fn a_merged_column_takes_the_issues_own_team_state() {
        let p = project();
        let status = status(&p);
        let todo = status
            .buckets()
            .unwrap()
            .into_iter()
            .find(|b| b.title == "Todo")
            .unwrap();
        assert_eq!(
            todo.key.as_deref(),
            Some("s-b-todo"),
            "the last Todo, TBB's"
        );
        let value = status
            .value_for_bucket(todo.key.as_deref().unwrap())
            .unwrap();
        assert_eq!(
            for_issue(&p, &issue("TBA", 1), status, value.clone()),
            Ok(select("s-a-todo", "Todo"))
        );
        assert_eq!(
            for_issue(&p, &issue("TBB", 2), status, value),
            Ok(select("s-b-todo", "Todo"))
        );
    }

    #[test]
    fn a_missing_state_is_refused_with_the_team_and_name() {
        let p = project();
        let r = for_issue(
            &p,
            &issue("TBA", 1),
            status(&p),
            select("s-b-review", "In Review"),
        );
        assert_eq!(
            r,
            Err(TeamRefusal::NoState {
                team: "TBA".into(),
                state: "In Review".into()
            })
        );
        assert_eq!(refusal(r), r#"TBA has no "In Review" state"#);
        let r = for_team(&p, &tbb(), status(&p), select("s-a-prog", "In Progress"));
        assert_eq!(refusal(r), r#"TBB has no "In Progress" state"#);
    }

    #[test]
    fn cycles_and_team_groups_belong_to_their_team() {
        let p = project();
        let cycles = field(&p, CYCLE_FIELD);
        assert_eq!(
            for_issue(&p, &issue("TBA", 1), cycles, cycle("c-now")),
            Ok(cycle("c-now"))
        );
        let r = for_issue(&p, &issue("TBB", 2), cycles, cycle("c-now"));
        assert_eq!(
            r,
            Err(TeamRefusal::OtherTeamsCycle {
                team: "TBB".into(),
                owner: "TBA".into()
            })
        );
        assert_eq!(refusal(r), "TBB issues can't be in TBA's cycles");

        let type_group = group(&p, "lg-type");
        let bug = select("l-bug", "Bug");
        assert_eq!(
            for_issue(&p, &issue("TBA", 1), type_group, bug.clone()),
            Ok(bug.clone())
        );
        let r = for_issue(&p, &issue("TBB", 2), type_group, bug);
        assert_eq!(
            r,
            Err(TeamRefusal::OtherTeamsGroup {
                group: "Type".into(),
                owner: "TBA".into()
            })
        );
        assert_eq!(refusal(r), r#""Type" belongs to team TBA"#);

        let size = group(&p, "lg-size");
        let small = select("l-small", "Small");
        for team in ["TBA", "TBB"] {
            assert_eq!(
                for_issue(&p, &issue(team, 1), size, small.clone()),
                Ok(small.clone()),
                "{team}"
            );
        }
    }

    #[test]
    fn a_cycle_the_board_does_not_know_needs_refresh() {
        let p = project();
        let r = for_issue(&p, &issue("TBA", 1), field(&p, CYCLE_FIELD), cycle("c-new"));
        assert_eq!(r, Err(TeamRefusal::NeedsRefresh));
    }

    #[test]
    fn duplicate_states_are_never_a_target() {
        let p = project();
        let dup = select("s-b-dup", "Dup");
        for team in ["TBA", "TBB"] {
            let r = for_issue(&p, &issue(team, 1), status(&p), dup.clone());
            assert_eq!(r, Err(TeamRefusal::Duplicate), "{team}");
        }
        assert_eq!(
            TeamRefusal::Duplicate.message(),
            "Mark duplicates in Linear"
        );
    }

    #[test]
    fn close_and_reopen_targets() {
        let p = project();
        assert_eq!(
            state_for(&p, &tba(), StateGoal::Completed),
            Ok(select("s-a-done", "Done"))
        );
        assert_eq!(
            state_for(&p, &tba(), StateGoal::Canceled),
            Ok(select("s-a-canceled", "Canceled"))
        );
        assert_eq!(
            state_for(&p, &tbb(), StateGoal::Completed),
            Ok(select("s-b-done", "Done"))
        );
        let r = state_for(&p, &tbb(), StateGoal::Canceled);
        assert_eq!(
            r,
            Err(TeamRefusal::NoStateOfKind {
                team: "TBB".into(),
                kinds: "canceled"
            })
        );
        assert_eq!(refusal(r), "TBB has no canceled state");
        assert_eq!(
            state_for(&p, &tba(), StateGoal::Reopen),
            Ok(select("s-a-todo", "Todo"))
        );
        assert_eq!(
            state_for(&p, &tbb(), StateGoal::Reopen),
            Ok(select("s-b-todo", "Todo"))
        );
    }

    #[test]
    fn reopen_falls_back_to_backlog_then_refuses() {
        use OptionKind::*;
        let mut p = project();
        p.fields[0] = select_field(
            STATUS_FIELD,
            "Status",
            vec![
                opt("s-a-backlog", "Backlog", Some(Backlog)),
                opt("s-a-later", "Later", Some(Backlog)),
                opt("s-b-done", "Done", Some(Completed)),
            ],
        );
        let owners = &mut p.linear.as_mut().unwrap().owners;
        owners.insert("s-a-backlog".into(), TBA_ID.into());
        owners.insert("s-a-later".into(), TBA_ID.into());
        assert_eq!(
            state_for(&p, &tba(), StateGoal::Reopen),
            Ok(select("s-a-backlog", "Backlog")),
            "the first backlog state in option order"
        );
        let r = state_for(&p, &tbb(), StateGoal::Reopen);
        assert_eq!(refusal(r), "TBB has no unstarted or backlog state");
        let r = state_for(&p, &tba(), StateGoal::Completed);
        assert_eq!(refusal(r), "TBA has no completed state");
    }

    #[test]
    fn the_status_cannot_be_cleared() {
        let p = project();
        assert_eq!(may_clear(&p, status(&p)), Err(TeamRefusal::StatusRequired));
        assert_eq!(
            TeamRefusal::StatusRequired.message(),
            "Every Linear issue has a status"
        );
        assert_eq!(may_clear(&p, field(&p, PRIORITY)), Ok(()));
        assert_eq!(may_clear(&p, field(&p, CYCLE_FIELD)), Ok(()));
        assert_eq!(may_clear(&p, group(&p, "lg-type")), Ok(()));
    }

    #[test]
    fn an_issue_of_a_team_not_on_the_board_says_so() {
        let p = project();
        let tbc = issue("TBC", 1);
        let not_on_board = Err(TeamRefusal::TeamNotOnBoard { team: "TBC".into() });
        let r = for_issue(&p, &tbc, status(&p), select("s-a-todo", "Todo"));
        assert_eq!(r, not_on_board);
        assert_eq!(refusal(r), "TBC is not one of this board's teams");
        let r = for_issue(&p, &tbc, field(&p, CYCLE_FIELD), cycle("c-now"));
        assert_eq!(r, not_on_board);
        let r = for_issue(&p, &tbc, group(&p, "lg-type"), select("l-bug", "Bug"));
        assert_eq!(r, not_on_board);
        let urgent = select("linear:priority:1", "Urgent");
        assert_eq!(
            for_issue(&p, &tbc, field(&p, PRIORITY), urgent.clone()),
            Ok(urgent)
        );
        let small = select("l-small", "Small");
        assert_eq!(
            for_issue(&p, &tbc, group(&p, "lg-size"), small.clone()),
            Ok(small),
            "a workspace group is no team's"
        );
    }

    #[test]
    fn a_board_cached_by_04_needs_refresh_for_every_team_owned_field() {
        let mut p = project();
        let meta = p.linear.as_mut().unwrap();
        meta.teams.clear();
        meta.owners.clear();
        let tba_issue = issue("TBA", 1);
        let r = for_issue(&p, &tba_issue, status(&p), select("s-a-todo", "Todo"));
        assert_eq!(r, Err(TeamRefusal::NeedsRefresh));
        assert_eq!(
            refusal(r),
            crate::model::Denial::NeedsRefresh.message(&|_| None)
        );
        // The board knows no teams, so it cannot say TBC is not one of them.
        let r = for_issue(&p, &issue("TBC", 1), status(&p), select("s-a-todo", "Todo"));
        assert_eq!(r, Err(TeamRefusal::NeedsRefresh));
        // Without owners nothing says which groups are the workspace's.
        let r = for_issue(
            &p,
            &tba_issue,
            group(&p, "lg-size"),
            select("l-small", "Small"),
        );
        assert_eq!(r, Err(TeamRefusal::NeedsRefresh));
        let urgent = select("linear:priority:1", "Urgent");
        assert_eq!(
            for_issue(&p, &tba_issue, field(&p, PRIORITY), urgent.clone()),
            Ok(urgent)
        );
    }

    #[test]
    fn other_fields_pass_through_for_any_team() {
        let p = project();
        let urgent = select("linear:priority:1", "Urgent");
        assert_eq!(
            for_team(&p, &tbb(), field(&p, PRIORITY), urgent.clone()),
            Ok(urgent)
        );
    }

    #[test]
    fn github_values_pass_through() {
        let mut p = crate::store::snapshot::tests::project();
        assert_eq!(p.linear, None);
        let gh_status = select_field(
            "PVTSSF_status",
            "Status",
            vec![opt("o-todo", "Todo", None), opt("o-todo-2", "Todo", None)],
        );
        // Even a field whose id looks like Linear's is GitHub's on a GitHub board.
        let lookalike = select_field(STATUS_FIELD, "Status", vec![opt("o-dup", "Dup", None)]);
        p.fields.push(gh_status.clone());
        p.fields.push(lookalike.clone());
        let item = crate::model::item::tests::issue("a", 1, "A");
        assert_eq!(issue_team(&p, &item), None);
        let todo = select("o-todo-2", "Todo");
        assert_eq!(
            for_issue(&p, &item, &gh_status, todo.clone()),
            Ok(todo.clone())
        );
        assert_eq!(for_issue(&p, &item, &lookalike, todo.clone()), Ok(todo));
        assert_eq!(may_clear(&p, &gh_status), Ok(()));
        assert_eq!(may_clear(&p, &lookalike), Ok(()));
    }

    #[test]
    fn column_team_is_the_single_owner() {
        let p = project();
        let status = status(&p);
        let buckets = status.buckets().unwrap();
        let bucket = |title: &str| buckets.iter().find(|b| b.title == title).unwrap();
        assert_eq!(column_team(&p, status, bucket("In Progress")), Some(&tba()));
        assert_eq!(column_team(&p, status, bucket("In Review")), Some(&tbb()));
        assert_eq!(column_team(&p, status, bucket("Todo")), None, "merged");
        assert_eq!(column_team(&p, status, bucket("No Status")), None);

        let cycles = field(&p, CYCLE_FIELD);
        let now = cycles
            .buckets()
            .unwrap()
            .into_iter()
            .find(|b| b.title == "c-now")
            .unwrap();
        assert_eq!(column_team(&p, cycles, &now), Some(&tba()));

        let priority = field(&p, PRIORITY);
        let urgent = &priority.buckets().unwrap()[0];
        assert_eq!(column_team(&p, priority, urgent), None, "no team's");

        let type_group = group(&p, "lg-type");
        let type_buckets = type_group.buckets().unwrap();
        let bug = type_buckets.iter().find(|b| b.title == "Bug").unwrap();
        assert_eq!(
            column_team(&p, type_group, bug),
            Some(&tba()),
            "TBA's group"
        );
        let no_type = type_buckets.last().unwrap();
        assert_eq!(no_type.key, None);
        assert_eq!(column_team(&p, type_group, no_type), None, "No Type");

        let size = group(&p, "lg-size");
        let small = &size.buckets().unwrap()[0];
        assert_eq!(column_team(&p, size, small), None, "the workspace's group");
    }
}
