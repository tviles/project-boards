#!/usr/bin/env bash
# Creates and seeds the public testbed used by the phase 0 probes, fixture recording and
# the live suite: repository tviles/project-boards-testbed and the project
# "project-boards testbed". Safe to re-run: every step reuses what already exists. Requires gh logged in as tviles with the
# repo and project scopes. Writes tests/testbed.toml.
set -euo pipefail
set -x # tracing; the token lives only in GH_TOKEN, never in command arguments
OWNER=tviles
REPO="$OWNER/project-boards-testbed"
TITLE="project-boards testbed"
root=$(cd "$(dirname "$0")/.." && pwd)

login=$(gh api user --jq .login)
if [ "$login" != "$OWNER" ]; then
  echo "gh is logged in as $login; run: gh auth switch --user $OWNER" >&2
  exit 1
fi
# --- Repository, labels, milestone -------------------------------------------------
gh repo view "$REPO" >/dev/null 2>&1 || gh repo create "$REPO" --public --add-readme \
  --description "Fixture data for github.com/tviles/project-boards"
gh label create bug --repo "$REPO" --color d73a4a --force
gh label create enhancement --repo "$REPO" --color a2eeef --force
gh label create docs --repo "$REPO" --color 0075ca --force
gh api "repos/$REPO/milestones" -f title=v1 >/dev/null 2>&1 || true

BODY=$'## Steps\n\n1. Open the board\n2. Press `L`\n\n- [x] done item\n- [ ] open item\n\n```rust\nfn main() {}\n```\n\n<details><summary>More</summary>hidden</details>\n\nSee #1 and https://example.com.'
issue() { # title labels [extra gh args...]  -> prints the URL of the lowest-numbered issue with that title, creating it if none
  local title=$1 labels=$2 url
  shift 2
  url=$(gh issue list --repo "$REPO" --state all --limit 200 --json number,title,url \
    --jq "[.[] | select(.title == \"$title\")] | sort_by(.number) | .[0].url // empty")
  if [ -z "$url" ]; then
    url=$(gh issue create --repo "$REPO" --title "$title" --body "$BODY" --label "$labels" "$@" | tail -n 1)
  fi
  echo "$url"
}

urls=()
urls+=("$(issue "Crash when board has no Status field" bug --assignee "$OWNER" --milestone v1)")
urls+=("$(issue "Support iteration columns" enhancement --milestone v1)")
urls+=("$(issue "Document fine-grained tokens" docs)")
urls+=("$(issue "Emoji title 🚀 with 中文 characters that is long enough to truncate" enhancement --assignee "$OWNER")")
for n in 5 6 7 8 9 10; do urls+=("$(issue "Filler issue $n" enhancement)"); done
closed_url=$(issue "Closed issue" bug)
if [ "$(gh issue view "$closed_url" --json state --jq .state)" = OPEN ]; then
  gh issue close "$closed_url" --reason completed
fi
urls+=("$closed_url")
if [ "$(gh issue view "${urls[0]}" --json comments --jq '.comments | length')" = 0 ]; then
  gh issue comment "${urls[0]}" --body "First comment with \`code\`."
  gh issue comment "${urls[0]}" --body "Second comment."
fi

# Close duplicates left by interrupted runs: open issues with a seeded title but another number.
for u in "${urls[@]}"; do
  n=${u##*/}
  t=$(gh issue view "$u" --json title --jq .title)
  for d in $(gh issue list --repo "$REPO" --state open --limit 200 --json number,title \
    --jq ".[] | select(.title == \"$t\" and .number != $n) | .number"); do
    gh issue close "$d" --repo "$REPO" --reason "not planned" --comment "Duplicate from an interrupted seed run"
  done
done

# --- A pull request ------------------------------------------------------------------
gh_git() { git -c credential.helper= -c 'credential.helper=!gh auth git-credential' "$@"; }
pr_url=$(gh pr list --repo "$REPO" --head feature/sample --state all --json url --jq '.[0].url // empty')
if [ -z "$pr_url" ]; then
  if ! gh api "repos/$REPO/branches/feature/sample" >/dev/null 2>&1; then
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    gh_git clone --quiet "https://github.com/$REPO.git" "$tmp/repo"
    (
      cd "$tmp/repo"
      git switch -c feature/sample
      echo "sample" > sample.txt
      git add sample.txt
      git commit -qm "Add sample file"
      gh_git push -q -u origin feature/sample
    )
  fi
  pr_url=$(gh pr create --repo "$REPO" --head feature/sample --title "Sample pull request" --body "Closes #2" | tail -n 1)
fi
urls+=("$pr_url")

# --- Project, link, fields -----------------------------------------------------------
# NOTE: the installed gh's `gh project` commands (other than list) fail with a GraphQL
# "Variable $query ... not declared" error, so everything else goes through gh api graphql.
existing=$(gh project list --owner "$OWNER" --limit 100 --format json --jq ".projects[] | select(.title == \"$TITLE\") | \"\\(.number) \\(.id)\"" | head -n 1)
if [ -n "$existing" ]; then
  read -r number project_id <<<"$existing"
  reused=1
else
  reused=0
  owner_id=$(gh api user --jq .node_id)
  read -r number project_id < <(gh api graphql \
    -f query='mutation($o:ID!,$t:String!){createProjectV2(input:{ownerId:$o,title:$t}){projectV2{id number}}}' \
    -f o="$owner_id" -f t="$TITLE" --jq '.data.createProjectV2.projectV2 | "\(.number) \(.id)"')
fi
repo_id=$(gh api "repos/$REPO" --jq .node_id)
# `|| true`: linking an already linked repository may be reported as an error.
gh api graphql -f query='mutation($p:ID!,$r:ID!){linkProjectV2ToRepository(input:{projectId:$p,repositoryId:$r}){repository{id}}}' \
  -f p="$project_id" -f r="$repo_id" >/dev/null || true

FIELDS_QUERY='query($p:ID!){node(id:$p){... on ProjectV2{fields(first:50){nodes{... on ProjectV2FieldCommon{id name} ... on ProjectV2SingleSelectField{options{id name}} ... on ProjectV2MultiSelectField{multiSelectOptions{id name}} ... on ProjectV2IterationField{configuration{iterations{id title}}}}}}}}'
fields() { gh api graphql -f query="$FIELDS_QUERY" -f p="$project_id" --jq "$1"; }
has_field() { local names; names=$(fields '.data.node.fields.nodes[].name'); grep -qxF -- "$1" <<<"$names"; }
fid() { fields ".data.node.fields.nodes[] | select(.name == \"$1\") | .id"; }
opt() { fields ".data.node.fields.nodes[] | select(.name == \"$1\") | (.options // .multiSelectOptions)[] | select(.name == \"$2\") | .id"; }
iter() { fields ".data.node.fields.nodes[] | select(.name == \"Sprint\") | .configuration.iterations[] | select(.title == \"$1\") | .id"; }

field() { # name type  (the type must not be SINGLE_SELECT, ITERATION or MULTI_SELECT)
  gh api graphql -f query='mutation($p:ID!,$n:String!,$t:ProjectV2CustomFieldType!){createProjectV2Field(input:{projectId:$p,dataType:$t,name:$n}){clientMutationId}}' \
    -f p="$project_id" -f n="$1" -f t="$2" >/dev/null
}
has_field Priority || gh api graphql -f query='mutation($p:ID!){createProjectV2Field(input:{projectId:$p,dataType:SINGLE_SELECT,name:"Priority",singleSelectOptions:[{name:"P0",color:RED,description:""},{name:"P1",color:ORANGE,description:""},{name:"P2",color:GRAY,description:""}]}){clientMutationId}}' \
  -f p="$project_id" >/dev/null
has_field Size || field Size NUMBER
has_field Due || field Due DATE
has_field Notes || field Notes TEXT

read -r s1 s2 s3 < <(python3 -c 'import datetime as d; t=d.date.today(); print(*(t+d.timedelta(days=14*i) for i in range(3)))')
if ! has_field Sprint; then
  gh api graphql -f query='mutation($p:ID!,$s1:Date!,$s2:Date!,$s3:Date!){createProjectV2Field(input:{projectId:$p,dataType:ITERATION,name:"Sprint",iterationConfiguration:{startDate:$s1,duration:14,iterations:[{startDate:$s1,duration:14,title:"Sprint 1"},{startDate:$s2,duration:14,title:"Sprint 2"},{startDate:$s3,duration:14,title:"Sprint 3"}]}}){clientMutationId}}' \
    -f p="$project_id" -f s1="$s1" -f s2="$s2" -f s3="$s3" >/dev/null
fi
if ! has_field Areas; then
  gh api graphql -f query='mutation($p:ID!){createProjectV2Field(input:{projectId:$p,dataType:MULTI_SELECT,name:"Areas",multiSelectOptions:[{name:"api",color:BLUE,description:""},{name:"ui",color:GREEN,description:""},{name:"docs",color:PURPLE,description:""}]}){clientMutationId}}' \
    -f p="$project_id" >/dev/null
fi

status_f=$(fid Status); priority_f=$(fid Priority); size_f=$(fid Size); due_f=$(fid Due)
notes_f=$(fid Notes); sprint_f=$(fid Sprint); areas_f=$(fid Areas)

# --- Items and values ----------------------------------------------------------------
ids=()
for u in "${urls[@]}"; do
  case "$u" in
    */pull/*) content_id=$(gh pr view "$u" --json id --jq .id) ;;
    *) content_id=$(gh issue view "$u" --json id --jq .id) ;;
  esac
  ids+=("$(gh api graphql -f query='mutation($p:ID!,$c:ID!){addProjectV2ItemById(input:{projectId:$p,contentId:$c}){item{id}}}' \
    -f p="$project_id" -f c="$content_id" --jq .data.addProjectV2ItemById.item.id)")
done
# ProjectV2.items lags fresh writes (index delay), so on a reused project wait until the
# items just added are visible; otherwise the draft lookup below could miss existing drafts
# and create duplicates. The archived closed issue is not counted by totalCount, hence the -1.
if [ "$reused" = 1 ]; then
  want=$((${#ids[@]} - 1))
  waited=0
  while :; do
    have=$(gh api graphql -f query='query($p:ID!){node(id:$p){... on ProjectV2{items(first:100){totalCount}}}}' \
      -f p="$project_id" --jq .data.node.items.totalCount)
    [ "$have" -ge "$want" ] && break
    if [ "$waited" -ge 120 ]; then
      echo "project items not indexed yet; wait a minute and re-run" >&2
      exit 1
    fi
    sleep 5
    waited=$((waited + 5))
  done
fi
DRAFTS_QUERY='query($p:ID!){node(id:$p){... on ProjectV2{items(first:100){nodes{id content{__typename ... on DraftIssue{title}}}}}}}'
draft() { # title body -> item id of the draft with that title, creating it if none
  local id
  id=$(gh api graphql -f query="$DRAFTS_QUERY" -f p="$project_id" \
    --jq "first(.data.node.items.nodes[] | select(.content.__typename == \"DraftIssue\" and .content.title == \"$1\") | .id)")
  if [ -z "$id" ]; then
    id=$(gh api graphql -f query='mutation($p:ID!,$t:String!,$b:String!){addProjectV2DraftIssue(input:{projectId:$p,title:$t,body:$b}){projectItem{id}}}' \
      -f p="$project_id" -f t="$1" -f b="$2" --jq .data.addProjectV2DraftIssue.projectItem.id)
  fi
  echo "$id"
}
draft1=$(draft "Draft idea" "A draft with **bold** text")
draft2=$(draft "Another draft" "")
ids+=("$draft1" "$draft2")

set_value() { # item field value-type value  (type: singleSelectOptionId, text, date or iterationId; all strings)
  local vt=String
  [ "$3" = date ] && vt=Date # a GraphQL variable's type must match the input field's
  gh api graphql -f query="mutation(\$p:ID!,\$i:ID!,\$f:ID!,\$v:$vt!){updateProjectV2ItemFieldValue(input:{projectId:\$p,itemId:\$i,fieldId:\$f,value:{$3:\$v}}){projectV2Item{id}}}" \
    -f p="$project_id" -f i="$1" -f f="$2" -f v="$4" >/dev/null
}
set_select() { set_value "$1" "$2" singleSelectOptionId "$3"; }
# 14 items: 11 issues (index 10 is the closed one), the PR (11), two drafts (12, 13).
# The last draft deliberately has no Priority, so boards grouped by Priority get a "No Priority" lane.
statuses=(Todo Todo "In Progress" "In Progress" Done Todo Todo "In Progress" Todo Todo Done "In Progress" Todo Todo)
priorities=(P0 P1 P2 P0 P1 P2 P0 P1 P2 P0 P1 P2 P0)
for i in "${!ids[@]}"; do
  set_select "${ids[$i]}" "$status_f" "$(opt Status "${statuses[$i]}")"
  if [ -n "${priorities[$i]:-}" ]; then
    set_select "${ids[$i]}" "$priority_f" "$(opt Priority "${priorities[$i]}")"
  fi
done
gh api graphql -f query='mutation($p:ID!,$i:ID!,$f:ID!,$n:Float!){updateProjectV2ItemFieldValue(input:{projectId:$p,itemId:$i,fieldId:$f,value:{number:$n}}){projectV2Item{id}}}' \
  -f p="$project_id" -f i="${ids[0]}" -f f="$size_f" -F n=3 >/dev/null
set_value "${ids[0]}" "$due_f" date "$s2"
set_value "${ids[0]}" "$notes_f" text "Needs a repro"
set_value "${ids[0]}" "$sprint_f" iterationId "$(iter "Sprint 1")"
set_value "${ids[1]}" "$sprint_f" iterationId "$(iter "Sprint 2")"
gh api graphql -f query='mutation($p:ID!,$i:ID!,$f:ID!,$a:String!,$b:String!){updateProjectV2ItemFieldValue(input:{projectId:$p,itemId:$i,fieldId:$f,value:{multiSelectOptionIds:[$a,$b]}}){projectV2Item{id}}}' \
  -f p="$project_id" -f i="${ids[0]}" -f f="$areas_f" -f a="$(opt Areas api)" -f b="$(opt Areas ui)" >/dev/null

# One archived item (the closed issue).
gh api graphql -f query='mutation($p:ID!,$i:ID!){archiveProjectV2Item(input:{projectId:$p,itemId:$i}){item{id}}}' \
  -f p="$project_id" -f i="${ids[10]}" >/dev/null

# --- Views ---------------------------------------------------------------------------
VIEWS_QUERY='query($p:ID!){node(id:$p){... on ProjectV2{views(first:20){nodes{id name}}}}}'
view() { # name layout -> id of the view with that name, creating it if none
  local id
  id=$(gh api graphql -f query="$VIEWS_QUERY" -f p="$project_id" --jq ".data.node.views.nodes[] | select(.name == \"$1\") | .id")
  if [ -z "$id" ]; then
    id=$(gh api graphql -f query='mutation($p:ID!,$n:String!,$l:ProjectV2ViewLayout!){createProjectV2View(input:{projectId:$p,name:$n,layout:$l}){projectV2View{id}}}' \
      -f p="$project_id" -f n="$1" -f l="$2" --jq .data.createProjectV2View.projectV2View.id)
  fi
  echo "$id"
}
board_view=$(view "Board" BOARD_LAYOUT)
bugs_view=$(view "Bugs" TABLE_LAYOUT)
view "Probe" TABLE_LAYOUT >/dev/null
gh api graphql -f query='mutation($v:ID!){updateProjectV2View(input:{viewId:$v,filter:"label:bug"}){clientMutationId}}' -f v="$bugs_view" >/dev/null
: "$board_view"

cat > "$root/tests/testbed.toml" <<EOF
owner = "$OWNER"
number = $number
project_id = "$project_id"
repo = "$REPO"
EOF

cat <<EOF
Seeded project #$number ($project_id). Three settings cannot be made through the API;
set them on github.com (https://github.com/users/$OWNER/projects/$number):
  1. View "Board": Column by Status, Group by Priority.
  2. View "View 1" (the default table): Group by Sprint, Sort by Priority descending.
  3. View "Bugs": show the Status, Priority and Assignees fields.
EOF
gh api graphql -f query='query($p:ID!){node(id:$p){... on ProjectV2{items(first:100){totalCount}}}}' \
  -f p="$project_id" --jq '"items: \(.data.node.items.totalCount) (may read low for a minute after seeding; re-check)"'
