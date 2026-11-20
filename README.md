# claude-code-alab

`claude-alab` runs [Claude Code](https://docs.anthropic.com/en/docs/claude-code)
and keeps it current while you work. When a new `claude` build is installed, the
running session moves onto it at the next quiet moment and picks up the same
conversation. Nothing to restart, nothing to retype.

It runs the unmodified vendor `claude` from the atpkg store. It never ships,
forks or patches vendor code.

```text
$ claude-alab                      # same as `claude`, kept up to date
$ claude-alab --model opus         # every argument goes to `claude` unchanged
$ claude-alab --version
claude-code-alab <version> (claude <vendor version>)
by Andrew Yates · ALab · alab.systems
```

Install Claude Code with `aterm pkg install claude`. Without it, `claude-alab`
says so and exits 127.

`claude-alab launch -- <arguments>` applies the managed launch defaults and
then execs Claude Code in the same process. It installs no supervisor and
restores any temporary prelaunch signal scope before the final exec. It leaves
aterm's live duties with aterm. This separate
entry lets launch policy move independently of full lifecycle qualification.

The usage query (`<host> harness tap --launch -- <arguments>`) tells the host
whether the launch's own standard output is a terminal
(`ATERM_LAUNCH_STDOUT_TTY=1` or `0`) and waits at most ten seconds for the
answer. aterm settles a folder's trust inside that query, within seven seconds
of its own, and settles none for a launch whose output is no terminal.

For a managed interactive launch whose original standard output is a terminal,
the owner also settles cold folder trust itself before asking for usage
settings. It first asks the host `harness launch-trust gate`, the host's
read-only folder-policy/session decision, and obeys the answer: only `allow`
goes on to the vendor, and a refusal is journaled. An aterm from before that
verb refuses it as words it does not know (exit 2, nothing on standard output);
the harness takes that for no gate, attests nothing and journals nothing, and
that host's usage query stays the one place trust is settled. On `allow`, the
selected vendor
receives one private `initialize.workspaceTrust` request with project hooks and
MCP servers disabled. There is no model prompt. Its five-second admission budget
is separate from the usage query's ten-second budget. A recorded result requires
actual native cohort settlement and a fresh read of the exact profile key.
Unsettled native custody prevents the vendor launch. Settled refusals leave
Claude Code's own dialog behavior intact. The completed observation is written
to this owner's journal; it does not manufacture a host approvals-ledger row.
Managed termination/stop signals during the cold operation request native
retirement before their original dispositions are restored and replayed.
Inherited ignores and blocked signals remain in force; resize and continue do
not cancel the launch. An unsupported original child-reaper policy declines
before any fork. This does not guarantee cleanup after SIGKILL or a machine failure.

`claude-alab trust folder --vendor <absolute-path> --stdout-tty 0|1 -- <arguments>`
uses the same operation for an explicitly selected vendor and environment.
It performs no fresh managed-build lookup. Native attestation refuses where
the original cohort cannot be safely owned; this is no interactive mode lease
or proof that arbitrary descendants which escaped the cohort have ended.

## What it does

**Runs Claude Code as its child on your terminal.** Arguments, environment,
working directory and signals pass straight through, and `claude-alab` exits
exactly the way `claude` did.

**Steps aside when it has nothing to add.** The harness stays with every
session it can move onto another build: one on a terminal, started with
arguments it can resume. Any other launch runs `claude` in the harness's own
place, as the same process, with nothing in between: no terminal on stdin, a
one-shot run (`-p`, `--help`, `--version`), flags that have no resume form
(`--worktree`, `--tmux`, a flag the harness does not know), or
`--alab-no-live-update`, which asks for a session it never moves. Only
your own word leaves it out of a session it could move: `--no-harness`, or
`ATERM_NO_HARNESS=1`, inside aterm. Nothing in aterm drives Claude Code in
its place: a session the harness leaves out has no crash relaunch, no live
update, no move off Fable's limit and no Autopilot.

**Updates Claude Code under the running session.** `aterm pkg update claude`
(or a rollback) changes which build is current. The harness notices, waits
until the session is idle, nothing is typed and no background work is running,
then ends the old build with `SIGTERM` and starts the new one with
`--resume <conversation>`. Launch flags are carried over, and the session
resumes in the conversation's permission mode where that is within the
launch's own. If the new build will not start, the session resumes on the old
one. A session that is busy is never interrupted.

**Updates itself.** The harness is a package too. When a new `claude-alab`
build is installed, the running harness replaces its own program image in
place and keeps the same `claude` child, so the session does not notice. If
the new image cannot take over, it returns to the old one.

**Moves between your accounts when aterm says so.** aterm keeps your
Claude accounts and decides which one a session runs on; the harness never
reads a token, never picks an account and asks aterm nothing about one. When
aterm moves the session onto another account, the harness resumes the same
conversation through aterm's token helper, which is named that account's key
and given its login, with no credential of the launch's, and every later
restart stays on that login. It ends `claude` for the move only at a moment
it judges safe itself (its own record of the session idle, or refused at a
usage limit it has read), and only within 15 s of aterm's request. A start
the helper will not make is undone, and the session goes on where it was. Fable's own limit moves the
model instead, on the same account.

**Continues on Opus when Fable runs out.** When a session on Fable is refused
at Fable's own usage limit, the harness ends `claude` with `SIGTERM` and
resumes the same conversation, on the same account, with `--model
claude-opus-5-5`. It types nothing into Claude Code's limit dialog. When the
limit has reset it resumes the conversation on the launch's model again,
also in a later harness process for the same conversation (a tab restored
after a restart of the Mac), which takes up the state the last one kept
beside the journal. A model a person chooses with `/model` while the session
is off Fable is kept, and no return is made over it. It decides this by itself, from the limit row Claude Code writes in the
conversation's transcript, so it does the same in every session it
supervises: outside aterm, inside it with or without aterm's claim, and
whatever the session's Autopilot is set to. A session launched on another
model is never moved. If aterm asks for the same move, the request is
journalled and no second restart is made.

**Works with aterm.** Inside an aterm tab the harness claims aterm's supervisor
contract and uses aterm's own view of the session (is the agent idle, is the
composer empty, did a person just type) to pick the moment to update. Under
that claim it also brings `claude` back on the same conversation if it
crashes, and restarts it when aterm asks. Without the claim it uses its own
readings of the terminal and Claude Code's session record, and behaves the
same anywhere else: where aterm refuses the claim or cannot be reached, the
harness still supervises the session (live updates and the move off Fable's
limit go on), asks aterm again on a back-off (5 s, then 15 s, 30 s and a
minute while a refusal stands), and says once in its journal that nothing is
typed in the tab until a claim stands; aterm's `status` then names no claim
of the harness's. The same holds for a claim aterm takes back during the
session (its policy turned off, a claim made again and refused). It types only through
aterm's fenced input: the tab's terminal is aterm's, not the harness's.

> **Status of the typing duties (2026-11-19).** The harness's claim takes the
> crash relaunch and the vendor upgrade (`relaunch`, `upgrade`) and, where
> aterm serves contract 4, the account moves (`accounts`, Decision 63), under
> every Autopilot word aterm's policy is in effect for. The turn ends, boxes,
> questions and walls described below are built and tested here but are
> **not claimed**: the shared link
> (`vendor/alab-harness`, `alab-wire`) has no fenced typing channel yet, so
> aterm's own engine keeps them, and the harness's journal says so once
> (`duty untyped shared-link …`). What follows about those duties, and about
> a claim that follows the Autopilot word, describes them as they will run
> once claimed (Decision 61).

**Carries the session's turn ends under Autopilot.** The claim also takes the
session's turn ends (`turn-end`), and the harness does at each one what the
tab's Autopilot says, in aterm's own words (with your standing rules):

What the claim takes follows the tab's Autopilot, and the claim is made
again as the word moves:

* **Cruise or Land**: the crash relaunch, the vendor upgrade and the duties
  that type (the turn ends, boxes, questions and walls the launch takes),
  with the account moves where aterm serves them. A launch's first claim,
  made before the word is read, and a claim made while aterm states no word
  for the session (`autopilot=-`), name the crash relaunch and the vendor
  upgrade alone, and nothing types the rest.
* **Off**: no claim. aterm's policy is off under Off (`effective=off
  why=autopilot-off`), so the harness claims nothing and gives back a claim
  that stood: a crash ends the session with its child, as outside aterm, no
  restart or account move aterm asks for reaches the harness (it hears them
  only under a claim), and nothing is typed in the tab. The live
  update and the move off Fable's limit and back go on, unclaimed. The
  policy is read again every 5 s, so the claim follows the word as soon as
  it is Cruise or Land again.

* **Off**: nothing.
* **Cruise**: the continuation at every turn end, and at each `Nothing
  left:` the done check with the next of Cruise's six kinds of work. It never
  parks.
* **Land**: Land's instruction, then its nudge, until the agent says it is
  done; then the land check. A check answered `LANDED` while git reads every
  repository the session works in clean (nothing changed, untracked,
  stashed, unpushed or off the remote main; a submodule's commits pushed),
  or answered with a `NEEDS YOU:` line, rests Land
  until a turn the harness did not type starts (yours, or the agent's own) or
  the tab's Autopilot word changes. A `LANDED` over work git still finds gets
  the nudge, naming what it found.

It types each act once, through aterm's fenced input, as the same claim,
never while you are typing or have something unsent in the composer, and
never into anything but a turn's end Claude Code's own reader vouches for: a
permission box, a limit wall, a busy turn, a fresh session and a turn you
interrupted get no turn-end act. A question that could consent to something
irreversible gets aterm's own answer each time it stands (Cruise: take the
option that deletes, overwrites and force-pushes nothing, and keep going;
Land: land the work, never force-push or delete it), never a continuation
that could read as consent, and never parks the session. Under Land every
question, choice or stop gets Land's answer in place of the instruction and
of every nudge, the nudge after the land check's answer included; a check
answered with a `NEEDS YOU:` line rests, typing nothing. A session it moved
off Fable's limit is told once to continue where it stopped.

**Answers the session's boxes and questions under Autopilot.** The claim also
takes the session's permission boxes (`approvals`) and question dialogs
(`questions`), each its own word. Under your policy's
`approve = "all"` the harness presses a box's one-shot allow, and a plan
box's yes that switches on no standing mode; under the session's
`questions=recommended` it answers a question with the option the agent marked
`(Recommended)`. It never chooses an answer that is kept for later asks, buys
something or grants more than the one call, so your launch's permission mode
is never widened. Each key is fenced on the screen it was decided from, and
a box you keyed on is yours until it goes. Everything else is left to you and
said once in the journal: every box under `approve = "none"` or `"safe"`, file
edits, writes and reads, the folder-trust, MCP and sign-in dialogs, Claude
Code's rm warning, a box your own settings send to a person, and a question
with no recommendation, several answers, your own typing in it, or words that
name a destructive act.

**Carries the session's walls under Autopilot.** The claim also takes the
walls a turn ends on (`walls`), under your policy's own switches and in
aterm's words:

* **An API error** (`retry_api_errors`): a reply cut off, or over its
  maximum, is tried again at once and then on a ladder; a refused request
  waits its own ladder; an outage (overloaded, a server error, no
  connection) is waited out in silence and tried blind on a doubling gap,
  a 429's `retry-after` honoured.
* **A usage limit** (`resume_limits`): nothing is typed until the limit
  resets (the reset the transcript names, a grace and a spread of up to a
  minute per session after it); then the agent is told to carry on. A limit
  Claude Code waits out by itself gets a later look, and a limit only usage
  credits lift is left while `usage_credits` is off. On the usage-limit
  dialog the harness presses `Wait here` (`limit_wait`), and nothing else:
  never stop, an upgrade, credits or a reset claim, and **never `Wait here`
  on Fable's limit, which the harness moves the session off** (above).
* **A full context** (`compact_on_context_wall`): `/compact`, once for the
  screen, again only on its ladder if the wall comes back. The autocompact
  offer is stepped to your `autocompact_offer` size and accepted.
* **A model switch's cost warning**: its one yes under `approve = "all"`,
  never a confirmation your own `PreModelSwitch` hook asked for.
* **The critical-memory banner**: the child is restarted on its
  conversation, only where the claim also takes `relaunch`, your policy's
  `relaunch` is on and aterm judges the moment safe, and no sooner than the
  restart back-off after the last one.
* **A lost login** is yours: signing in opens your browser, so `/login` is
  never typed. It is said once in the journal.

Every wall act is fenced like a turn-end act, never typed while you type or
have something unsent, never on a screen Claude Code's reader does not vouch
for, and never twice on a screen that did not change. A usage limit's resume
is a turn-end act in aterm's books: a claim that takes `walls` and not
`turn-end` types none, and leaves it to the person.

## Options

Options for the harness come first and start with `--alab-`. Everything after
them is passed to `claude`.

| Option | Effect | Default |
|---|---|---|
| `--alab-no-live-update` | Never move this session: run `claude` alone, in the harness's place | |
| `--alab-quiet-ms=<ms>` | How long the session must be idle before an update | 20000 |
| `--alab-poll-ms=<ms>` | How often the store and the session are checked | 250 |
| `--alab-term-grace-ms=<ms>` | How long an update waits for `claude` to end | 30000 |
| `--alab-reset-grace-ms=<ms>` | How long after Fable's limit resets a session moved onto Opus goes back | 60000 |
| `--alab-duties=<list>` | Which duties the launch names (`relaunch`, `upgrade`, `turn-end`, `approvals`, `questions`, `walls`). The claim takes `relaunch` and `upgrade` whatever the list says; the four that type are not claimed yet (the shared link has no fenced typing channel), so they stay aterm's and the journal says so once (`duty untyped shared-link …`) | all six |
| `--alab-help` | Show the usage text | |

This build leads `claude`: `alab-harness.toml` declares `leads = ["claude"]`,
`--alab-leads=claude` answers yes, and aterm's launcher hands a session typed
as `claude` to `claude-alab run -- <arguments>`, which applies the launch
policy and supervises the vendor as its child. `--alab-launches=claude`
advertises `claude-alab launch -- <arguments>`, the entry that prepares the
launch and execs the vendor in place with no supervisor.
Inside aterm, `claude --no-harness` (`ATERM_NO_HARNESS=1`) runs `claude` alone.

Both managed entries own Claude Code's launch defaults: `--yolo`, the default
permission flags, `--no-harness`, and the package update commands. Inside an
aterm tab it also selects the fullscreen renderer unless you chose one, asks
for the usage tap and routes the vendor through the account's token service.
The launcher supplies its host executable as `ATERM_HARNESS_HOST`; the harness
never searches `PATH` for it. A direct `claude-alab` launch without that variable
uses the store and terminal itself, with no tap or account-service helper.
The usage and account services still live in aterm; their invocation and the
Claude launch policy now live in this repository.

On a contract 3 host that advertises native child proof and a pre-spawn barrier, the harness
announces a fresh generation before each initial start, restart, vendor update,
and fallback. It confirms the pending claim before spawning and then binds the
exact native child lifetime to that generation. A request reconnect retains a
continuing child's generation and known native identity. The identity is also
retained on hosts that serve child proof without the generation extension. An
unavailable or different native observation defers reclaim; the original owned
child's confirmed end or the next explicit start boundary retires that knowledge.
The pre-spawn status, begin and readback steps share one original 500 ms deadline.
If that bounded exchange fails, it drops the claim and starts Claude without
asserting authority; older hosts keep their existing launch behavior.

On an admitted contract 4 stream, the harness announces the vendor child's
kernel identity and sends the model name, effort level and top-effort key that
its transcript states. It sends the current snapshot again when the stream
reopens. The records identify their child so the host can refuse a replay
from a replaced child. Own-image transfer inside aterm remains unqualified.

The same stream carries a `session` record at every start, model move and
account move: the conversation, its directory, the account, the vendor
version and the words that resume it after `claude-alab run --`. These words
are the harness's own resume line, without the usage tap's `--settings`. When
the harness is gone, aterm types them. While the session is off Fable for its
limit, the words name no model, so the new harness goes back at the reset.
Words aterm would refuse to type are not sent, and aterm then uses its own
snapshot. A vendor update sends `upgrade` records in order: `seen` when
`current` names the build, `waiting` while the safe point holds, `moving` at
the signal, and then `done` or `abandoned`. Each record is sent once per
change, only while the stream is open. Both are sent again when a stream
opens on an aterm that has forgotten them (Decision 62).

The metadata-only `--alab-launches=<vendor-word>` and
`--alab-leads=<vendor-word>` queries describe launch policy ownership and full
supervision separately. They open no store, host, terminal or vendor state.
The source declarations are also the package's consistency boundary:
`alab-harness.toml` names the same leading word.

`claude-alab prime settings-defaults --home /absolute/home --config /absolute/config`
initializes Claude's unset settings without starting Claude. The home supplies
`.claude.json` compaction choices; the configuration directory supplies
`settings.json`. Existing effort, survey and compaction choices take precedence.
The command preserves settings order, backs up replaced bytes and refuses busy,
invalid or oversized files. `--alab-primes=claude:settings-defaults` is its separate
metadata capability query. It grants no supervision or permission-mode authority.
The command returns one closed versioned result row with `prime=1`, the operation,
and `changed` and `backup` booleans; it includes no settings names or paths.

`claude-alab prime legacy-hooks --home /absolute/home --config /absolute/config`
removes earlier aterm hook adapters and restores the displaced status line while
keeping other settings. It runs independently of defaults initialization. Its
separate query is `--alab-primes=claude:legacy-hooks`; a clean file creates no lock
or backup. The closed `prime=1` result records `affected` and `backup=none` or a
lowercase hex encoding of the absolute backup path's native bytes. Backup paths
are bounded at 64 KiB and the complete row at 131200 bytes, without truncation or
lossy UTF-8 conversion. This operation grants no supervision or mode authority.

`claude-alab inspect settings-defaults --home /absolute/home --config /absolute/config --auto-prime=1`
reports those same defaults without changing anything. Use `--auto-prime=0` when
automatic initialization is off. The separate metadata request is
`--alab-inspects=claude:settings-defaults`; initialization capability does not imply
inspection capability. No vendor, account service or network request is started.

The response is one complete LF-terminated JSON document with `schema: 1`,
`operation: "settings-defaults"`, the explicit `auto_prime` boolean, `paths` and
`findings`. Each settings/state path has lossless native-byte `selected_hex`,
optional `physical_hex`, and `status` (`present`, `missing`, `broken-link` or
`unavailable`). Physical paths describe a read; they authorize no action.
Each finding has a unique stable `key`, `disposition` (`ok`, `change` or
`unavailable`), `class` (`person` or `preference`), `label`, `detail`, nullable
`expected`, and `remedy` (`kind`, nullable `target` and nullable `instruction`).
The only edit target is `settings`; a remedy explains an edit and executes none.
The host can display these rows without interpreting Claude's settings schema.
Presentable strings contain no terminal controls or line separators. The read
budgets are 1 MiB for settings and 2 MiB for state; paths are at most 64 KiB,
findings at most eight, and the complete response at most 1 MiB. The response is
validated before stdout; an oversized response is refused without partial JSON.
Inspection creates no directory, lock, backup or temporary file. It grants no
supervision or permission-mode authority.

## The journal

Every decision the harness makes is one line in
`~/.local/state/claude-code-alab/journal.log` (or under `$XDG_STATE_HOME`):
each start, each update it saw, why it is waiting, each relaunch, each
refusal. When an update is not happening, the reason is the last `wait` line.
A launch that ran `claude` alone ends in one line, `pass <reason>`.

It also names the model, effort level and top-effort key that `claude` states
in its transcript, one line per change: `facts model=Opus_5.5 effort=high
key=off`, with `-` for a fact not stated yet or not readable. That line is
their only output for now; aterm will be told them once the harness interface
carries them.

An unchanged transcript stamp skips the read. Every changed version replays
at most the last 256 KiB of whole rows over the facts from when this conversation
was entered. Facts that window may have skipped become unread, even if an
earlier look saw them. This bounds each read and avoids retaining statements
that were rewritten or removed. Metadata checks before and after the read
reject observed changes; they do not make a concurrent read atomic.

A Fable limit and what the harness decided about it is one line (`limit-seen
bucket=… resets-at=… decision=move to=claude-opus-5-5`, or `decision=stay
<why>`), and each move is one line (`model-move to=… from=… cause=… kept=…
back-at=…`). A move that is waiting says why once (`limit-move wait …`), and
one that could not be made says so with its pause (`limit-move failed …
retry-in-ms=…`). A model chosen in the session while it is off Fable is
`limit-move given-up chose=…`, and a session taken up off Fable from an
earlier process is `limit-adopted back-at=… walled-returns=…`.

## Status

This comparison separates implementation ownership from host adoption. The cost
column describes process and I/O bounds; no CPU, startup or latency measurements
are claimed. The host column identifies the code that remains until its replacement
handoff is accepted.

| Feature | Independent implementation | Structural cost | Remaining aterm ownership |
|---|---|---|---|
| First launch | [Launch policy](src/launch.rs) owns argument/default handling, renderer selection, update interception and usage/token helper invocation. | `launch --` execs the vendor in the same process and leaves no resident supervisor or poll loop. | Managed routing still needs the owner handoff; usage and account-token services remain in aterm. |
| Relaunch and update | The [host link adapter](src/supervise_link.rs) uses the canonical link for claims, policy, events, child generations and reconnects. Claude keeps its crash and replacement decisions. | Claim/reconnect state lives in the shared lifecycle machine. Eligible `run --` sessions retain a supervisor. | Existing lifecycle consumers remain for paths without those claims. Typing duties, walls and task state still belong to aterm; full leadership is not advertised. |
| Unset defaults | [Priming](src/prime.rs) owns Claude's settings schema and preserves selected effort, survey and compaction choices. | A direct operation on the explicitly selected files starts no vendor child. | The host initializer and its Claude defaults table remain until delegation is accepted. |
| Legacy hook cleanup | [Cleanup](src/prime/legacy_hooks.rs) owns adapter matching and status-line restoration through the shared settings transaction code. | Nothing managed to remove means no lock or write; replaced bytes retain a backup. | The host's Claude settings editor and cleanup remain until the owner callback handoff is accepted. |
| Read-only settings findings | [Inspection](src/inspect.rs) uses the same interpretation as priming and returns owner-authored findings. | Reads are bounded at 1 MiB for settings and 2 MiB for state; the response is at most 1 MiB. It creates no files. | Doctor still interprets Claude settings locally until it consumes the independent findings. |
| Model, effort and key | The [transcript reader](src/model_facts.rs) keeps the three facts separate, including their unread states, across conversation changes and reports them on admitted contract 4 streams. | Each changed transcript replays at most 256 KiB. An unchanged stamp skips payload reads. | Aterm consumes the record when its build supports child-bound model records. Existing footer readers cover sessions without an applied record. |
| Resume words and move phases | The [host link adapter](src/supervise_link.rs) sends `session` at each start, model move and account move, and the vendor move's `upgrade` phases, on admitted contract 4 streams. | One resume line per start, conversation, model or account change, and per turn end. A pending build's `.vendor` record is read once. Nothing is read without a stream. | aterm's watchdog types the newest `session` record's words. Without one, it uses its own snapshot. `upgrade` replaces the engine's row while the stream feeds it. |
| Shared dependencies | The [canonical pin](vendor/alab-harness.pin) names the selected tag and tree for shared policy and wire. The [sealed projections](vendor/VENDORED) own only phase readers and agent names. | Canonical policy/link code is reused; screen readers are generated copies, with an independently chosen canonical pin. | The screen grammar is still produced by aterm. Moving that dependency out is part of the remaining extraction. |
| Fable's limit | [The limit's decider](src/limit.rs) reads the child's own usage-limit rows, resumes the conversation on Opus 5.5 at Fable's limit and goes back after the reset, under the claim's `relaunch` or unclaimed. A model aterm names in a restart is kept for every later resume line. | One read of the transcript's new rows per look; one restart per move, backed off when it cannot be made. | None for the move. The resume a moved session is owed at its wall is a turn-end act, which stays aterm's. |
| Account moves | The [host link adapter](src/supervise_link.rs) claims `accounts` under contract 4 where the launch reaches aterm's token helper. A restart aterm sends that names an account's key ends the child at the harness's own safe point and resumes the conversation through aterm's token helper with [the account's login and no credential variable](src/account.rs); the claim names the account once the vendor runs (Decision 63). | One policy and status read per request, asked again every second for 15 s at most; one restart per move. | aterm picks the account, keeps the tokens and bounds the moves. Its token helper refuses a login that is not the account's, and its watchdog must type no `--account` for this harness. |
| Typing duties | The deciders ([turn end](src/duty.rs), [boxes](src/boxes.rs), [walls](src/walls.rs)) and their drivers are built and tested, and are not claimed: the [host link adapter](src/supervise_link.rs) claims no typing duty. | Nothing is read or typed for a duty that is not claimed. | aterm's own engine keeps Autopilot's turn ends, the boxes, the questions and the walls until the shared link carries a fenced, counted exchange on the claim's connection. |

The [standalone implementation](src/supervise.rs) also includes signal and exit
status forwarding, vendor-update fallback to the previous build, and self-update
outside aterm.

A conversation switch can read more than one transcript; the 256 KiB bound is
per changed file, not a whole-look I/O ceiling. Metadata and discovery work remain
even when a payload read is skipped. Live host consumers must stay until their
replacement is implemented and qualified.

Earlier integration suites exercised the then-current launch and lifecycle
behavior against scripted vendor and host stand-ins. A separate synthetic native
aterm run also covered the claim, crash recovery, vendor upgrade, and the host's
approval, question and Cruise behavior with the independent harness. A separate
different-image run passed
all seven local-provider cases: busy upgrade, failed exec, rollback, expired
entry, held handoff, held roles and malformed adoption. These historical runs do
not qualify real Claude Code, accounts, quota display or an own-image update
inside aterm.

Not done yet:

* **Full supervision through the default `claude` handoff is on and is being
  qualified in daily use**, not by a finished native qualification: real
  Claude Code, accounts, the quota display and an own-image update inside
  aterm are unproven. Older installed aterm builds also lack the
  child-generation boundary.
* **A crash is relaunched only under a proven aterm claim.** Without one,
  the harness exits with `claude`'s status.
* **Account services** still execute in aterm (the pool, the logins and the
  tokens). A box or a wall the harness leaves to the person is the person's:
  nothing else presses it. `approve = "safe"` presses nothing here yet:
  aterm's read-only proofs are not ported. Of
  aterm's wall rules the harness keeps the narrower ones: it does not measure
  the API's route, so an outage is tried blind and never confirmed cleared;
  it never renews a login (`logins = "renew"`), spends a limit reset
  (`limit_reset`) or consents to usage credits.
* **The walls duty has not met a real Claude Code or a real aterm.** It
  passes against the scripted host and vendor over aterm-phase's measured
  screens. An installed aterm must grant a claim that takes `walls`, keep
  serving the wall switches in its policy reply, and judge the wall point for
  the memory restart.
* **The box duties have not met a real Claude Code or a real aterm.** They
  pass against the scripted host and vendor over aterm-phase's measured
  screens. An installed aterm must grant a claim that takes `approvals` or
  `questions` and judge `key if-gen= if= act=`.
* **The turn-end duty has not met a real Claude Code or a real aterm.** It
  passes against the scripted host and vendor. An installed aterm must state
  the session's `autopilot=` to the claim and announce a change of the
  session's word for the harness to act; aterm's main states `-` for a Claude
  Code session this harness leads until its sweep publishes the tab's program,
  and until then the harness claims no typing duty, and nothing types. Land
  asks git whether the repositories' work is on the remote main branch before
  it rests, and reports the rest to aterm (`landed@v1`, `left-to-person@v1`),
  but does not yet hint the land check with what is left.
* **Self-update inside aterm** remains unqualified and held.
* **External suspension of Claude Code 2.1.325** can leave its terminal
  modes changed after `fg`. The real-vendor control reproduces this without
  a harness; Ctrl-Z resumes correctly. The leading-command qualification
  remains unmet.
* **The move off Fable's limit has not met a real limit.** The limit row is
  read as Claude Code 2.1.323 wrote one on 2026-11-12, and the moves pass
  against the scripted vendor. Not measured on a real Claude Code: what its
  session record says while the limit dialog is open (the move waits while it
  says `busy`, for at most ten minutes), that `--model` on a resume overrides the conversation's
  model, and that `SIGTERM` at the dialog ends it cleanly.
* **The resumed session is told to go on only under the claim.** With
  Autopilot Cruise or Land the harness types one resume after the move; with
  Autopilot off, or outside aterm, it types nothing and the conversation sits
  on Opus, idle, until a person sends the next turn.
* **The return at a turn's end has not been measured under Cruise.** Under
  aterm's claim the return is made in the second and a half after a turn
  ends. The harness's own turn-end duty types nothing into that window while
  the return is due, and is never typed over: past the window it continues
  the session, and the return waits for the act to be taken up and for the
  next turn end. Where the window is missed at every turn, the session stays
  on Opus until it is idle for the quiet period.
* **An empty composer and a hidden one read alike.** At the limit dialog
  aterm reports `composer=-`, which also means a composer it cannot tell
  from a draft; under a claim the move is made on it once the person's grace
  has passed, so a draft aterm could not read, left that long, is lost with
  the child it was typed into.
* **No fallback where the harness is not the child's parent.** A launch that
  runs `claude` in the harness's own place (no terminal, a one-shot run,
  `--alab-no-live-update`) has no harness left to move it.
* **A model changed with `/model`** comes back as the launch flags set it
  after an update or a crash relaunch; the journal names it, and nothing
  carries it, except while the session is off Fable: there a choice gives
  the return up and is kept. A choice made in the picker or with `/fast on`
  names no model word, so on a launch that names a model the resume names
  that model instead.
* **A launch under another `HOME` makes no own-image move.** A handed-over
  launch finds the vendor under the launcher's own prefix, but the harness's
  own builds are looked for under `HOME`.
* **Model transcript discovery is bounded.** One look may spend 4096 directory
  reads, including the read that proves enumeration ended. An incomplete,
  unreadable or ambiguous lookup leaves the facts unread and retries at most
  once a second. A known transcript path needs no directory scan.
* **Linux** follows the same rules but has never been run. On 2026-11-14 all
  targets were type-checked and linted (stable clippy, `-D warnings`) for
  `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`. macOS is the
  measured platform.

Where this is going: aterm drawing the session natively instead of as a grid of
terminal characters, and the harness's custody of the session moving into
shared ALab system services.

## Headless SDK observations

`sdk_permission::Session` owns a headless SDK child and its private input and
output pipes. It correlates each explicit application response with an
unpredictable request ID and the original child's exact native executable
generation. Missing, duplicate, late or uncertain responses stop the channel;
partial writes are retained and never retried. Output has one aggregate limit.

The developer-only `sdk_permission` test image runs the real protocol client.
aterm-lab's `protocol-proof` supplies a private jail, a local API stub and image
checks; SDK requests and mode interpretation remain in this repository. These
are SDK observations, with no prompt or model request. They do not establish an
interactive TUI mode provider or grant input/restart authority. Exact native
exec observation currently requires macOS; unsupported platforms refuse.

## Building

The repository pins the Trust toolchain (`rust-toolchain.toml`).

```sh
targo --unverified build --locked
targo --unverified test --locked
targo tippy --locked --all-targets -- -D warnings
targo fmt --check
```

The tests run the real binary on a real pseudo-terminal against a scripted
stand-in for Claude Code, and against a stand-in for aterm's control socket.

`vendor/alab-harness` holds one whole qualified tag of the canonical shared
harness repository, named by `vendor/alab-harness.pin`. Both `alab-policy`
and `alab-wire` build from that tree. Update it with the canonical repository's
`tools/pin <this checkout> <tag>`, after that tag passes its own gate, then
commit the whole copy and pin and run its `tools/check-pin <this checkout>`.
Stage the copy and pin before running vendor-out. Its version can advance
independently of aterm's pin; aterm's own repin never changes this one.

aterm's `verify vendor-out` generates only `aterm-phase` and
`aterm-agent-names`, including their standalone manifests and the sealed
`vendor/VENDORED` digest list. Phase uses the same canonical policy dependency
as this package. The projection checker validates the separate canonical owner
before reserving exactly its tree and pin; every other unlisted vendor file
remains an error. Generated files are changed through their producers.
`tests/pin.rs` checks the committed canonical tree and working copy in a Git
checkout. An unpacked archive has no committed tree to compare; the upstream
tag/content comparison still requires the release check in its original checkout.

## License

Apache-2.0; see [LICENSE](LICENSE).
