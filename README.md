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

For a managed interactive launch whose original standard output is a terminal,
the owner can settle cold folder trust before asking for usage settings. The
host supplies a read-only folder-policy/session decision; the selected vendor
receives one private `initialize.workspaceTrust` request with project hooks and
MCP servers disabled. There is no model prompt. Its five-second admission budget
is separate from the usage query's two-second budget. A recorded result requires
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

**Steps aside when it has nothing to add.** The harness stays only with a
session it can move onto another build: one on a terminal, started with
arguments it can resume. Any other launch runs `claude` in the harness's own
place, as the same process, with nothing in between: no terminal on stdin, a
one-shot run (`-p`, `--help`, `--version`), or flags that have no resume form
(`--worktree`, `--tmux`, a flag the harness does not know).

**Updates Claude Code under the running session.** `aterm pkg update claude`
(or a rollback) changes which build is current. The harness notices, waits
until the session is idle, nothing is typed and no background work is running,
then ends the old build with `SIGTERM` and starts the new one with
`--resume <conversation>`. Launch flags and the session's permission mode are
carried over. If the new build will not start, the session resumes on the old
one. A session that is busy is never interrupted.

**Updates itself.** The harness is a package too. When a new `claude-alab`
build is installed, the running harness replaces its own program image in
place and keeps the same `claude` child, so the session does not notice. If
the new image cannot take over, it returns to the old one.

**Works with aterm.** Inside an aterm tab the harness claims aterm's supervisor
contract and uses aterm's own view of the session (is the agent idle, is the
composer empty, did a person just type) to pick the moment to update. Under
that claim it also brings `claude` back on the same conversation if it
crashes, and restarts it when aterm asks. Without the claim it uses its own
readings of the terminal and Claude Code's session record, and behaves the
same anywhere else.

## Options

Options for the harness come first and start with `--alab-`. Everything after
them is passed to `claude`.

| Option | Effect | Default |
|---|---|---|
| `--alab-no-live-update` | Never move this session: run `claude` alone, in the harness's place | |
| `--alab-quiet-ms=<ms>` | How long the session must be idle before an update | 20000 |
| `--alab-poll-ms=<ms>` | How often the store and the session are checked | 250 |
| `--alab-term-grace-ms=<ms>` | How long an update waits for `claude` to end | 30000 |
| `--alab-help` | Show the usage text | |

`--alab-launches=claude` advertises `claude-alab launch -- <arguments>` as the
managed host entry. That entry prepares the launch and execs the vendor in
place. Host adoption and installed routing require their own qualification.
Full supervision would declare
`--alab-leads=claude` and use `claude-alab run -- <arguments>`; this build keeps
that declaration disabled until native qualification is complete.
Inside aterm, `ATERM_NO_HARNESS=1` runs `claude` alone too.

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

The metadata-only `--alab-launches=<vendor-word>` and
`--alab-leads=<vendor-word>` queries describe launch policy ownership and full
supervision separately. They open no store, host, terminal or vendor state.
No leading word is enabled until native qualification is complete; the source
declarations are also the package's consistency boundary.

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
A launch that ran `claude` alone is one line, `pass <reason>`.

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
| Shared dependencies | The [canonical pin](vendor/alab-harness.pin) names the selected tag and tree for shared policy and wire. The [sealed projections](vendor/VENDORED) own only phase readers and agent names. | Canonical policy/link code is reused; screen readers are generated copies, with an independently chosen canonical pin. | The screen grammar is still produced by aterm. Moving that dependency out is part of the remaining extraction. |

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

* **Full supervision through the default `claude` handoff** waits for complete native qualification.
  Older installed aterm builds also lack the child-generation boundary.
* **A crash is relaunched only under a proven aterm claim.** Without one,
  the harness exits with `claude`'s status.
* **Approvals, questions, continuations, walls and account services** still
  execute in aterm. They need their own extraction and qualification before
  the corresponding host code can be deleted.
* **Self-update inside aterm** remains unqualified and held.
* **External suspension of Claude Code 2.1.325** can leave its terminal
  modes changed after `fg`. The real-vendor control reproduces this without
  a harness; Ctrl-Z resumes correctly. The leading-command qualification
  remains unmet.
* **A model changed with `/model`** comes back as the launch flags set it
  after an update or a crash relaunch; the journal names it, and nothing
  carries it.
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
