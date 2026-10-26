---
tags: [on-call, incidents, handbook, operations]
---
# On-call handbook for the platform team

This handbook is for everyone on the platform team's on-call rotation, from people about to take their first shift to people who have done it for years and want to check what changed. It describes how the rotation works, what is expected during a shift, how to handle a page, how to run an incident, and how to hand over. It is deliberately practical. The rules here exist because each of them was missing at some point and its absence hurt; where that history is useful, it is mentioned. If something in this handbook is wrong or outdated, fixing it is part of the on-call job.

#### The rotation

##### Shape of a shift

The rotation has a primary and a secondary for each week, from Tuesday at 10:00 to the following Tuesday at 10:00 local time. Handover on a Tuesday morning rather than a Monday was chosen on purpose: Monday is the busiest day for deploys and for leftover weekend issues, and handing over in the middle of it meant the outgoing person left exactly when context was most needed. The primary gets every page first. If the primary does not acknowledge within ten minutes, the page escalates to the secondary, and after another ten minutes to the engineering manager. The secondary is also the person the primary calls for help, and a primary who has been working on an incident for more than an hour must call the secondary, even at night.

###### Compensation and limits

On-call time is compensated according to the company policy, and pages outside working hours are additionally compensated per incident. More important than compensation are the limits. Nobody does more than one primary week in four, nobody is on call during approved leave, and anyone who was paged for more than two hours between midnight and 07:00 starts work late the next day without needing to ask. Before these limits existed, a few experienced people ended up carrying most of the night pages because they resolved them fastest, and two of them left the team within a year. The limits are not negotiable, including by the people they protect, who sometimes try to waive them.

#### Before your shift

##### Access check

The week before your primary shift, run the access check, which verifies that you can do everything on-call might require: log in to every production cluster, read logs and traces, use the deploy tool's rollback, open the database consoles for the platform's own databases, access the secrets store for break-glass credentials, and receive a test page on your phone. Run it with:

```sh
oncall-preflight --role platform-primary
```

The access check exists because of an incident in which the primary was paged at 03:00 for a full disk on a database node, and discovered that their access to the database hosts had expired the week before during a routine access review. They spent forty minutes getting emergency access approved while the disk filled up. Since then, access expiry for on-call roles is aligned with the rotation schedule, and the preflight is mandatory.

###### Read the last handover

Read the handover notes from the last two shifts and look at the open incident follow-ups. Most pages during a shift are related to something that happened in the previous two weeks, and knowing that a particular alert has been flaky, or that a migration is running on a particular cluster, saves a lot of time when the page arrives.

#### Handling a page

##### First five minutes

When a page arrives, acknowledge it immediately, even before you look at it, so that it does not escalate to the secondary while you are opening your laptop. Then open the alert's linked dashboard, not a general dashboard; every alert links to the dashboard of the affected service, which shows its indicators, dependencies and recent changes on one screen. Within the first five minutes, try to answer four questions: what is broken, since when, who is affected, and what changed recently. Write the answers, even if incomplete, into the alert's channel thread. This is not bureaucracy: written notes are what lets the secondary or anyone else join without asking you to repeat everything, and they are the raw material for the postmortem.

###### Deciding whether it is an incident

Not every page is an incident. A page is an incident if users are affected now or will be soon, if data is at risk, or if you need help from anyone outside the rotation. If none of these is true, handle the page, write a short note in the thread and move on. If any of them is true, declare an incident with the incident tool, which creates a dedicated channel, assigns you as incident commander by default and posts a status page draft. Declaring an incident that turns out to be minor costs a few minutes; not declaring one that turns out to be major costs much more, because the coordination structure is missing exactly when it is needed. When in doubt, declare. During the first year of the current process, about a third of declared incidents were later downgraded to minor, and we consider that a healthy ratio.

##### Mitigation before diagnosis

The first goal during an incident is to stop the harm, not to understand it. If a deploy happened shortly before the problem started, roll it back without waiting to confirm that it is the cause; rollback is cheap and quick, and confirming causation can take an hour. If a feature flag was changed, revert it. If one node or zone is misbehaving, take it out of rotation. Diagnosis comes after the bleeding has stopped. New on-call engineers often feel that rolling back without proof is unprofessional, and the opposite is true: in our incident history, rollbacks that turned out to be unnecessary have never caused harm, while delayed rollbacks have extended several incidents by tens of minutes. Example of a rollback with the deploy tool:

```sh
deployctl rollback --service ingress-gateway --to previous --reason "INC-2291 error spike after deploy"
```

New members of the rotation often ask what a typical week actually looks like, and the honest answer is that most weeks are quiet and a few are not, so it helps to describe both. In a quiet week the primary gets perhaps three to five pages, most of them during working hours, and spends the rest of the time on follow-up work from previous shifts: tuning an alert that fired for no good reason, fixing a runbook step that turned out to be wrong, or writing the missing mitigation note for a change that could not be rolled back. The secondary in a quiet week is mostly not involved at all, apart from the handover call and perhaps one consultation. In a busy week, which happens maybe once every two months, there is a real incident with user impact, often at an inconvenient hour, and the primary spends most of a day on it, including the postmortem preparation that follows. The handbook rules about compensation and limits exist mainly for those weeks, and new members should not feel that taking the late start after a night page is a sign of weakness: it is how the rotation stays sustainable over years rather than months. A completely different question that comes up just as often is how to learn the systems well enough to be useful on call. Reading documentation helps, but the most effective way we have found is shadowing: before their first primary week, every new member spends one full week as a third person on the rotation, receiving every page in parallel with the primary, joining every incident channel and asking questions afterwards, without any responsibility for acting. After the shadow week they take a primary week with an experienced secondary who is explicitly asked to be more available than usual. People who skipped the shadow week because of scheduling pressure consistently reported their first primary week as much more stressful, so we no longer allow skipping it. Another practice that helps is the monthly game day, a one-hour session where someone breaks something in the staging environment on purpose, such as filling a disk, killing a database replica or misconfiguring a load balancer, and the current primary diagnoses it with the rest of the team watching. Game days are deliberately low-stakes and are often funny, but they have found real gaps more than once: the most memorable was a game day where nobody could find the dashboard for the message queue cluster, because its link had broken during a migration of the dashboard system weeks earlier and nobody had needed it since. Going back to the quiet weeks, one thing that experienced members do differently is that they use the quiet time to look at the alert history for patterns rather than waiting for the weekly review. An alert that fired four times in a week, each time resolving on its own within five minutes, is easy to miss in the review summary, but it is often an early sign of a capacity problem that will become a real incident a month later. The handbook does not require this, but it is the habit that most clearly separates people who have been on the rotation for years from those who have just joined. We are also experimenting with a short weekly note from the outgoing primary to the whole team, not only to the incoming primary, summarising anything that looked like the beginning of a trend, so that the knowledge does not stay with the two people on the handover call. The first few of these notes have already prompted two capacity tickets that would otherwise have waited for the quarterly planning. There is one more kind of week worth mentioning, which is the week of a large planned change, such as a database migration or a major upgrade of a shared cluster. Those weeks are scheduled with the rotation in mind: the change is announced at least a week ahead, the change author is on standby during and after the change, and the primary for that week is told explicitly what to watch and when to call the author. The rotation does not own planned changes, and the primary should never be the person executing a large change during their shift, because the two roles compete for attention exactly when something goes wrong. If the only person who can execute a change is on call that week, the change moves to another week or the on-call shift is swapped.

###### When rollback is not possible

Some changes cannot be rolled back simply: database migrations that dropped a column, configuration changes to external providers, data that has already been written in a new format. For changes like these, the change itself must come with a documented mitigation, and the deploy tool shows that mitigation note when someone attempts a rollback. If you hit a change without a rollback path and without a mitigation note, call the change author immediately, regardless of the time. That is an explicit expectation for every engineer who ships irreversible changes, and it is in the change process for that reason.

#### Running an incident

##### Roles

For any incident bigger than one person can handle, split the roles. The incident commander coordinates: decides what to try next, makes sure someone is working on each line of investigation, and keeps track of time. The commander does not debug. The communications lead posts status updates to the incident channel every thirty minutes and to the public status page when customers are affected, and handles questions from support and management so that responders are not interrupted. Responders investigate and act. In a small incident, the primary is commander and responder and the secondary handles communications; in a large one, the commander should be someone who is not hands-on, and the primary should hand over the commander role as soon as such a person joins.

###### Status updates

Status updates follow a fixed format: current impact, what we know, what we are doing, and when the next update will be. The last item matters most. A stakeholder who knows that the next update comes in thirty minutes does not ask for one in ten. Updates should be honest about uncertainty: "we do not yet know the cause; we are rolling back the 14:05 deploy as a precaution" is a good update, while silence for an hour followed by a confident root cause is not.

##### Ending an incident

An incident ends when user impact has ended and the system is stable, not when the cause is understood. The commander declares the end in the incident channel, the communications lead posts a resolution on the status page, and the commander assigns a postmortem owner. Postmortems are required for every incident with user impact longer than fifteen minutes or any data loss, and are held within five working days, while memory is fresh. They are blameless: the purpose is to understand how the system allowed the incident, and the document must not name individuals as causes.

###### Talking to other teams

Many platform incidents involve another team's service, either as cause or as victim. When you need another team, page their on-call through the incident tool rather than messaging people you know directly; the tool records who was paged and when, and it respects their escalation policy. Messaging a friend on another team at night bypasses their rotation, puts the burden on someone who may not be on call, and leaves no record for the postmortem. If the other team's on-call does not respond within their escalation window, escalate to their manager through the tool, without guilt.

#### Noise and alert hygiene

A rotation is only sustainable if pages are rare and meaningful. Every page during a shift is recorded with a classification: actionable and urgent, actionable but could have waited, not actionable, or false alarm. At the weekly on-call review, every page that was not actionable and urgent gets a follow-up: tune the alert, change it to a ticket instead of a page, or delete it. Over the last year this review reduced the average number of pages per week from twenty-three to seven, and night pages from about six per week to one or two. The most effective single change was converting alerts on resource saturation, such as CPU and memory, into tickets, and keeping pages only for symptoms that users feel: error rates, latency and data freshness.

###### Flaky alerts

An alert that fires and resolves on its own several times a week trains people to ignore it, and eventually someone ignores it when it is real. Any alert that self-resolves more than three times in a week is automatically flagged in the on-call review, and the default action is to silence it until it is fixed, with a ticket assigned to the owning team. Silencing an alert feels risky, but a flaky alert that everyone ignores already provides no protection; silencing it at least makes that explicit.

#### Handover

At the end of the shift, the outgoing primary writes a handover note and spends fifteen minutes on a call with the incoming primary. The note lists incidents and notable pages with links, anything still in progress, alerts that were flaky, any temporary changes made during the shift that need to be undone, such as silenced alerts, scaled-up resources or manual overrides, and anything the next shift should watch. Temporary changes are the most important item: in a past incident, a manual scale-up made during a shift was never reverted and was forgotten, and when the autoscaler configuration was later changed, the manual override silently prevented it from working, which turned a minor traffic spike into an outage weeks later. Example of the handover template's temporary changes section:

```text
Temporary changes still in place:
- alert "etcd-latency-high" silenced until 2026-03-21 (ticket PLAT-4410)
- ingress-gateway min replicas raised 6 -> 10 (revert after traffic test, owner: secondary)
```

##### After your shift

Take the day after a heavy shift easy if you can, and use some of your next week to do the follow-ups you found: fix the runbook that was wrong, tune the alert that woke you up for nothing, write the missing mitigation note. The rotation gets better only when people who experienced its problems spend time fixing them. The engineering manager tracks on-call follow-ups as part of the team's regular work, not as a side activity, and there is reserved capacity for them in every sprint.
