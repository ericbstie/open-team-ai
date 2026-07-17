# A team is a runtime entity, not a label

Forming a team creates two mechanical things: a routable `team:<id>` message scope
(members auto-subscribed on join, unsubscribed on leave) and a task-assignment
scope (tasks taggable to the team, with membership as the claim-eligibility
filter). Dissolving a team is an event that releases both. A bookkeeping-only
label was rejected because it would make "dynamically formed teams" decorative.
